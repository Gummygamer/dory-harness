// SCRATCH: earliest graph node whose per-token values change when later tokens join the batch.
use dory_sys::*;
use std::{ffi::{CStr, CString, c_char, c_void}};
#[link(name = "ggml-base")]
unsafe extern "C" {
    fn ggml_backend_tensor_get(t: *const ggml_tensor, data: *mut c_void, offset: usize, size: usize);
    fn ggml_nbytes(t: *const ggml_tensor) -> usize;
    fn ggml_op_desc(t: *const ggml_tensor) -> *const c_char;
}
struct N { name: String, op: String, ne: [i64; 4], nb: [usize; 4], ty: u32, data: Vec<u8>, srcs: Vec<String> }
struct Cap { v: Vec<N>, done: bool }
unsafe extern "C" fn cb(t: *mut ggml_tensor, ask: bool, user: *mut c_void) -> bool {
    let c = unsafe { &mut *(user as *mut Cap) };
    if ask { return !c.done; }
    if c.done { return true; }
    let name = unsafe { CStr::from_ptr((*t).name.as_ptr()) }.to_string_lossy().into_owned();
    let op = unsafe { CStr::from_ptr(ggml_op_desc(t)) }.to_string_lossy().into_owned();
    let n = unsafe { ggml_nbytes(t) };
    let mut data = vec![0u8; n];
    unsafe { ggml_backend_tensor_get(t, data.as_mut_ptr() as *mut c_void, 0, n) };
    let tt = unsafe { &*t };
    let srcs = tt.src.iter().filter(|s| !s.is_null()).map(|s| unsafe { CStr::from_ptr((**s).name.as_ptr()) }.to_string_lossy().into_owned()).collect();
    let mut ne=[0i64;4]; let mut nb=[0usize;4]; for i in 0..4 { ne[i]=tt.ne[i]; nb[i]=tt.nb[i]; }
    c.v.push(N { name: name.clone(), op, ne, nb, ty: tt.type_ as u32, data, srcs });
    if name == "l_out-0" { c.done = true; }
    true
}
fn key(k: &str) -> [c_char; 128] { let mut o = [0 as c_char; 128]; for (d, s) in o.iter_mut().zip(k.bytes()) { *d = s as c_char; } o }
unsafe fn run(model: *mut llama_model, toks: &[i32], tail: usize) -> Vec<N> { unsafe {
    let mut cap = Box::new(Cap { v: vec![], done: false });
    let mut cp = llama_context_default_params();
    cp.n_ctx = 4096; cp.n_batch = 2048; cp.n_ubatch = 512; cp.n_seq_max = 1; cp.swa_full = false; cp.n_outputs_max = 1; cp.n_threads = 8; cp.n_threads_batch = 8;
    if std::env::var("DORY_NOFA").is_ok() { cp.flash_attn_type = llama_flash_attn_type_LLAMA_FLASH_ATTN_TYPE_DISABLED; }
    if std::env::var("DORY_KV_F32").is_ok() { cp.type_k = ggml_type_GGML_TYPE_F32; cp.type_v = ggml_type_GGML_TYPE_F32; }
    if std::env::var("DORY_TRUE_CPU").is_ok() { cp.op_offload = false; cp.offload_kqv = false; }
    cp.cb_eval = Some(cb); cp.cb_eval_user_data = &mut *cap as *mut Cap as *mut c_void;
    let ctx = llama_init_from_model(model, cp); assert!(!ctx.is_null());
    let mut p = toks.to_vec();
    if tail > 0 {
        let k = p.len() - tail;
        assert_eq!(llama_decode(ctx, llama_batch_get_one(p.as_mut_ptr(), k as i32)), 0);
        llama_synchronize(ctx); cap.v.clear(); cap.done = false;
        assert_eq!(llama_decode(ctx, llama_batch_get_one(p.as_mut_ptr().add(k), tail as i32)), 0);
    } else {
        assert_eq!(llama_decode(ctx, llama_batch_get_one(p.as_mut_ptr(), p.len() as i32)), 0);
    }
    llama_synchronize(ctx); llama_free(ctx); cap.v
}}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let toks: Vec<i32> = std::fs::read_to_string(&a[5]).unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
    let (n, m) = (toks.len(), toks.len() / 2);
    unsafe {
        llama_backend_init();
        unsafe extern "C" fn quiet(_: ggml_log_level, _: *const c_char, _: *mut c_void) {}
        llama_log_set(Some(quiet), std::ptr::null_mut());
        let mut mp = llama_model_default_params(); mp.n_gpu_layers = a[4].parse().unwrap();
        let mut no_dev: [ggml_backend_dev_t; 1] = [std::ptr::null_mut()];
        if std::env::var("DORY_TRUE_CPU").is_ok() { mp.devices = no_dev.as_mut_ptr(); mp.n_gpu_layers = 0; }
        let mut o1: llama_model_kv_override = std::mem::zeroed(); o1.tag = llama_model_kv_override_type_LLAMA_KV_OVERRIDE_TYPE_INT; o1.key = key("dory.num_loops"); o1.__bindgen_anon_1.val_i64 = a[2].parse().unwrap();
        let mut o2: llama_model_kv_override = std::mem::zeroed(); o2.tag = llama_model_kv_override_type_LLAMA_KV_OVERRIDE_TYPE_STR; o2.key = key("dory.recurrent_kv_cache_mode"); for (d, s) in o2.__bindgen_anon_1.val_str.iter_mut().zip(a[3].bytes()) { *d = s as c_char; }
        let ov = [o1, o2, std::mem::zeroed()]; mp.kv_overrides = ov.as_ptr();
        let p = CString::new(a[1].as_str()).unwrap();
        let model = llama_model_load_from_file(p.as_ptr(), mp); assert!(!model.is_null());
        let tail: usize = std::env::var("TAIL").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let w = run(model, &toks, 0); let q = if tail > 0 { run(model, &toks, tail) } else { run(model, &toks[..m], 0) };
        let (m, base) = if tail > 0 { (tail, n - tail) } else { (m, 0) };
        println!("nodes captured whole={} prefix={} n={n} m={m}", w.len(), q.len());
        for (x, y) in w.iter().zip(q.iter()) {
            if x.name != y.name { println!("{:40} name mismatch vs {}", x.name, y.name); continue; }
            // token axis: the axis whose extent is n in whole and m in prefix
            let d = (0..4).rev().find(|&d| x.ne[d] == n as i64 && y.ne[d] == m as i64);
            let shape = format!("{:?}", x.ne);
            let Some(d) = d else { println!("{:28} {:16} {:26} (no token axis)", x.name, x.op, shape); continue; };
            if x.ty != 0 || (0..4).any(|i| i != d && x.ne[i] != y.ne[i]) { println!("{:28} {:16} {:26} (type {} or shape differs)", x.name, x.op, shape, x.ty); continue; }
            let st = x.nb[d];
            if st != y.nb[d] || st * n != x.data.len() { println!("{:28} {:16} {:26} (non-contiguous)", x.name, x.op, shape); continue; }
            let (mut mx, mut at, mut first) = (0f32, 0usize, None);
            for r in 0..m {
                let (ax, bx) = (&x.data[(base+r)*st..(base+r+1)*st], &y.data[r*st..(r+1)*st]);
                for (ca, cb) in ax.chunks_exact(4).zip(bx.chunks_exact(4)) {
                    let (fa, fb) = (f32::from_le_bytes(ca.try_into().unwrap()), f32::from_le_bytes(cb.try_into().unwrap()));
                    let dd = (fa - fb).abs();
                    if dd > mx { mx = dd; at = r; }
                    if dd > 0.0 && first.is_none() { first = Some(r); }
                }
            }
            println!("{:28} {:16} {:26} max|d|={:.3e} at_row={:3} first_diff_row={:?} src={:?}", x.name, x.op, shape, mx, at, first, x.srcs);
        }
    }
}
