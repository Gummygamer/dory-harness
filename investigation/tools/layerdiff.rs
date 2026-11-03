// SCRATCH: per-layer hidden state comparison across prompt chunkings.
use dory_sys::*;
use std::{collections::BTreeMap, ffi::{CStr, CString, c_char, c_void}};
#[link(name = "ggml-base")]
unsafe extern "C" {
    fn ggml_backend_tensor_get(t: *const ggml_tensor, data: *mut c_void, offset: usize, size: usize);
    fn ggml_nbytes(t: *const ggml_tensor) -> usize;
}
#[derive(Default)]
struct Cap { m: BTreeMap<String, (usize, Vec<f32>)> } // name -> (row width, data)
unsafe extern "C" fn cb(t: *mut ggml_tensor, ask: bool, user: *mut c_void) -> bool {
    let name = unsafe { CStr::from_ptr((*t).name.as_ptr()) }.to_string_lossy().into_owned();
    let want = std::env::var("SWEEP").is_err() && (name.starts_with("l_out") || name.starts_with("kqv_out"));
    if ask { return want; }
    if want {
        let n = unsafe { ggml_nbytes(t) };
        let mut v = vec![0f32; n / 4];
        unsafe { ggml_backend_tensor_get(t, v.as_mut_ptr() as *mut c_void, 0, n) };
        let c = unsafe { &mut *(user as *mut Cap) };
        c.m.insert(name, (unsafe { (*t).ne[0] } as usize, v));
    }
    true
}
fn key(k: &str) -> [c_char; 128] { let mut o = [0 as c_char; 128]; for (d, s) in o.iter_mut().zip(k.bytes()) { *d = s as c_char; } o }
struct Run { per_chunk: Vec<BTreeMap<String, (usize, Vec<f32>)>>, logits: Vec<f32> }
unsafe fn run(model: *mut llama_model, toks: &[i32], chunks: &[usize], threads: i32) -> Run {
    unsafe {
        let mut cap = Box::new(Cap::default());
        let mut cp = llama_context_default_params();
        cp.n_ctx = 4096; cp.n_batch = 2048; cp.n_ubatch = 512; cp.n_seq_max = 1; cp.swa_full = false; cp.n_outputs_max = 1;
        cp.n_threads = threads; cp.n_threads_batch = threads;
        if std::env::var("DORY_NOFA").is_ok() { cp.flash_attn_type = llama_flash_attn_type_LLAMA_FLASH_ATTN_TYPE_DISABLED; }
        if std::env::var("DORY_KV_F32").is_ok() { cp.type_k = ggml_type_GGML_TYPE_F32; cp.type_v = ggml_type_GGML_TYPE_F32; }
        if std::env::var("DORY_TRUE_CPU").is_ok() { cp.op_offload = false; cp.offload_kqv = false; }
        cp.cb_eval = Some(cb); cp.cb_eval_user_data = &mut *cap as *mut Cap as *mut c_void;
        let ctx = llama_init_from_model(model, cp); assert!(!ctx.is_null());
        let mut per_chunk = vec![]; let mut off = 0;
        for &c in chunks {
            let mut part = toks[off..off + c].to_vec(); off += c;
            cap.m.clear();
            assert_eq!(llama_decode(ctx, llama_batch_get_one(part.as_mut_ptr(), c as i32)), 0);
            llama_synchronize(ctx);
            per_chunk.push(std::mem::take(&mut cap.m));
        }
        let nv = llama_vocab_n_tokens(llama_model_get_vocab(model)) as usize;
        let logits = std::slice::from_raw_parts(llama_get_logits_ith(ctx, -1), nv).to_vec();
        llama_free(ctx);
        Run { per_chunk, logits }
    }
}
fn il(name: &str) -> i32 { name.rsplit('-').next().and_then(|s| s.parse().ok()).unwrap_or(-1) }
// max abs difference for rows [ra..ra+n) of a vs [rb..rb+n) of b, per tensor name, in layer order
fn table(a: &BTreeMap<String, (usize, Vec<f32>)>, ra: usize, b: &BTreeMap<String, (usize, Vec<f32>)>, rb: usize, n: usize, label: &str) {
    let mut names: Vec<_> = a.keys().filter(|k| b.contains_key(*k)).cloned().collect();
    names.sort_by_key(|k| (il(k), k.clone()));
    let mut first = None; let mut lo = vec![];
    for k in &names {
        let (w, va) = &a[k]; let (_, vb) = &b[k];
        let (mut mx, mut na) = (0f32, 0f32);
        for r in 0..n { for c in 0..*w {
            let (x, y) = (va[(ra + r) * w + c], vb[(rb + r) * w + c]);
            mx = mx.max((x - y).abs()); na = na.max(x.abs());
        } }
        if k.starts_with("l_out") { lo.push((il(k), mx, na)); }
        if first.is_none() && mx > 1e-4 * na.max(1e-9) { first = Some((k.clone(), mx, na)); }
    }
    println!("{label}: first tensor differing by >1e-4 relative: {first:?}");
    let s: Vec<String> = lo.iter().filter(|(i, _, _)| i % 8 == 0 || *i < 8).map(|(i, m, _)| format!("{i}:{m:.1e}")).collect();
    println!("  l_out max|d| by layer: {}", s.join(" "));
    if let Some(l) = lo.last() { println!("  last layer {} max|d|={:.3e} (max|x|={:.3})", l.0, l.1, l.2); }
}
fn top2(l: &[f32]) -> String {
    let mut i: Vec<usize> = (0..l.len()).collect(); i.sort_by(|a, b| l[*b].partial_cmp(&l[*a]).unwrap());
    format!("top {}={:.4} {}={:.4}", i[0], l[i[0]], i[1], l[i[1]])
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (path, loops, mode, ngl, tokf, threads) = (&a[1], a[2].parse::<i64>().unwrap(), &a[3], a[4].parse::<i32>().unwrap(), &a[5], a[6].parse::<i32>().unwrap());
    let toks: Vec<i32> = std::fs::read_to_string(tokf).unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
    let n = toks.len(); let m = n / 2;
    unsafe {
        llama_backend_init();
        unsafe extern "C" fn quiet(_: ggml_log_level, _: *const c_char, _: *mut c_void) {}
        llama_log_set(Some(quiet), std::ptr::null_mut());
        let mut mp = llama_model_default_params(); mp.n_gpu_layers = ngl;
        let mut no_dev: [ggml_backend_dev_t; 1] = [std::ptr::null_mut()];
        if std::env::var("DORY_TRUE_CPU").is_ok() { mp.devices = no_dev.as_mut_ptr(); mp.n_gpu_layers = 0; }
        let mut o1: llama_model_kv_override = std::mem::zeroed(); o1.tag = llama_model_kv_override_type_LLAMA_KV_OVERRIDE_TYPE_INT; o1.key = key("dory.num_loops"); o1.__bindgen_anon_1.val_i64 = loops;
        let mut o2: llama_model_kv_override = std::mem::zeroed(); o2.tag = llama_model_kv_override_type_LLAMA_KV_OVERRIDE_TYPE_STR; o2.key = key("dory.recurrent_kv_cache_mode"); for (d, s) in o2.__bindgen_anon_1.val_str.iter_mut().zip(mode.bytes()) { *d = s as c_char; }
        let ov = [o1, o2, std::mem::zeroed()]; mp.kv_overrides = ov.as_ptr();
        let p = CString::new(path.as_str()).unwrap();
        let model = llama_model_load_from_file(p.as_ptr(), mp); assert!(!model.is_null());
        println!("n={n} m={m} loops={loops} {mode} ngl={ngl} threads={threads}");
        if std::env::var("SWEEP").is_ok() {
            let w = run(model, &toks, &[n], threads);
            println!("whole {}", top2(&w.logits));
            for t in [1usize,2,3,4,5,6,7,8,12,16,24,31,32,33,48,64,96,128,137,138,200,256] {
                if t >= n { continue; }
                let r = run(model, &toks, &[n - t, t], threads);
                let mx = r.logits.iter().zip(&w.logits).map(|(a,b)|(a-b).abs()).fold(0f32,f32::max);
                println!("tail={t:4} max|dlogit|={mx:.5} {}", top2(&r.logits));
            }
            return;
        }
        let whole = run(model, &toks, &[n], threads);
        let whole2 = run(model, &toks, &[n], threads);
        let prefix = run(model, &toks[..m], &[m], threads);
        let split = run(model, &toks, &[m, n - m], threads);
        let tail = run(model, &toks, &[n - 4, 4], threads);
        println!("logits whole: {}", top2(&whole.logits));
        println!("logits split m: {}", top2(&split.logits));
        println!("logits split n-4: {}", top2(&tail.logits));
        table(&whole.per_chunk[0], 0, &whole2.per_chunk[0], 0, n, "REPEAT whole vs whole (noise floor)");
        table(&whole.per_chunk[0], 0, &prefix.per_chunk[0], 0, m, "CAUSALITY rows<m: whole vs prefix alone");
        table(&whole.per_chunk[0], m, &split.per_chunk[1], 0, n - m, "CHUNK rows>=m: whole vs second chunk");
        table(&whole.per_chunk[0], n - 4, &tail.per_chunk[1], 0, 4, "CHUNK last 4: whole vs n-4 split");
    }
}
