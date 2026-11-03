"""Hypothesis test: the gap comes from the server's checkpoint split of the prompt.
A: server with --ctx-checkpoints 0 vs saved harness output (single chunk).
B: harness with --batch n-4 vs saved default-server output."""
import glob, json, os, subprocess, sys, time, urllib.request
# Paths come from this repo's location: <parent>/dory-harness, with matrix dirs next to it.
D=os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
M=os.path.dirname(os.path.dirname(D))
SAVED=M+'/src/matrix'; NOCK=M+'/src/matrix_nockpt'
sys.path.insert(0,D+'/scripts')
from compare_reference import post, server_messages, server_tools
CFG=[(1,'last_loop'),(2,'last_loop'),(4,'last_loop'),(2,'per_loop'),(4,'per_loop')]
CASES=sorted(glob.glob(D+'/scripts/gen_cases/*.json'))
URL='http://127.0.0.1:18080'
def wait():
    for _ in range(90):
        try:
            if b'ok' in urllib.request.urlopen(URL+'/health',timeout=2).read(): return True
        except Exception: pass
        time.sleep(2)
    return False
print('=== A: server --ctx-checkpoints 0 vs saved harness', flush=True)
for loops,mode in CFG:
    d=f'{NOCK}/{loops}_{mode}'; os.makedirs(d,exist_ok=True)
    log=open(f'{NOCK}/server_{loops}_{mode}.log','w')
    p=subprocess.Popen([M+'/src/llama.cpp-dory/build/bin/llama-server','-m',M+'/models/dory/NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf','--host','127.0.0.1','--port','18080','-ngl','999','--fit','off','-c','4096','-np','1','--jinja','--reasoning','on','--temp','1.0','--top-p','0.95','--top-k','0','--min-p','0','--repeat-penalty','1','--ctx-checkpoints','0','--override-kv',f'dory.num_loops=int:{loops},dory.recurrent_kv_cache_mode=str:{mode}'],stdout=log,stderr=log)
    assert wait(), 'server did not start'
    same=tot=0; chat_same=0
    for path in CASES:
        name=os.path.basename(path)[:-5]; case=json.load(open(path)); think=case.get('enable_thinking',True)
        tools=server_tools(case.get('tools',[])) or None
        prompt=post(URL+'/apply-template',{'messages':server_messages(case['messages']),'tools':tools,'chat_template_kwargs':{'enable_thinking':think}})['prompt']
        c=post(URL+'/completion',{'prompt':prompt,'n_predict':600,'temperature':0,'top_k':1,'top_p':1.0,'min_p':0.0,'cache_prompt':False,'n_probs':5})
        ch=post(URL+'/v1/chat/completions',{'messages':server_messages(case['messages']),'tools':tools,'chat_template_kwargs':{'enable_thinking':think},'temperature':0,'top_k':1,'top_p':1.0,'min_p':0.0,'max_tokens':600,'cache_prompt':False})['choices'][0]['message']
        json.dump({'prompt':prompt,'raw':c['content'],'message':ch,'probs':c.get('completion_probabilities') or []},open(f'{d}/{name}.json','w'))
        got=json.load(open(f'{SAVED}/{loops}_{mode}/{name}.dory.json'))
        eq=got['raw']==c['content']; tot+=1; same+=eq
        r=(ch.get('reasoning_content') or '').strip(); gr=got['reasoning'].strip(); ceq=(r==gr) and (ch.get('content') or '').strip()==got['content'].strip(); chat_same+=ceq
        print(f'  {loops} {mode} {name}: completion==harness {eq}  chat==harness {ceq}',flush=True)
    print(f'A {loops} {mode}: completion {same}/{tot}, chat {chat_same}/{tot}',flush=True)
    p.terminate(); p.wait(); time.sleep(4)
print('=== B: harness split n-4 + 4 vs saved default server',flush=True)
for loops,mode in CFG:
    same=tot=0
    for path in CASES:
        name=os.path.basename(path)[:-5]
        sv=json.load(open(f'{SAVED}/{loops}_{mode}/{name}.dory.json')); ref=json.load(open(f'{SAVED}/{loops}_{mode}/{name}.json'))
        n=sv['prompt_tokens']
        out=subprocess.run([D+'/target/release/dory-cli','--loops',str(loops),'--cache-mode',mode,'--ctx','4096','--batch',str(n-4),'--top-logprobs','5','raw',path,'--max-tokens','600','--temp','0'],capture_output=True,text=True)
        if out.returncode: print('  ERR',loops,mode,name,out.stderr.strip().splitlines()[-1:],flush=True); tot+=1; continue
        got=json.loads(out.stdout.strip().splitlines()[-1])
        os.makedirs(f'{NOCK}/split',exist_ok=True); json.dump(got,open(f'{NOCK}/split/{loops}_{mode}_{name}.json','w'))
        eq=got['raw']==ref['raw']; tot+=1; same+=eq
        print(f'  {loops} {mode} {name} n={n}: split harness==default server {eq}',flush=True)
    print(f'B {loops} {mode}: {same}/{tot}',flush=True)
print('CHUNK_TEST_DONE',flush=True)
