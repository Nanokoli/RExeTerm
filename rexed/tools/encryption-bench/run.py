from pathlib import Path
import subprocess,threading,signal,json,time,platform,os
T=Path(__file__).resolve().parent
(T/'evidence').mkdir(exist_ok=True)
SERVER=Path(os.environ.get('REXED_BIN', str(T.parent.parent/'target/release/rexed')))
CLIENT=Path(os.environ.get('RE_BENCH_BIN', str(T/'target/release/re-encryption-bench')))
config=T/'benchmark-config'; runtime=T/'benchmark-runtime';runtime.mkdir(exist_ok=True)
env={**os.environ,'XDG_RUNTIME_DIR':str(runtime)}
subprocess.run([str(SERVER),'--config-dir',str(config),'init'],check=True,env=env)
subprocess.run([str(SERVER),'--config-dir',str(config),'user','bench','--password-stdin','--allow','execute,files-read,files-write,pty-view,pty-control,forward'],input=b'benchmark-password\n',check=True,env=env)
results=[]
for scheme in ['tcp','tls','ws','wss']:
    args=[str(SERVER),'--id',f'encryption-{scheme}','--listen',f'{scheme}://127.0.0.1:0','--config-dir',str(config),'--admin-listen',f'unix:///tmp/re-encryption-{os.getpid()}-{scheme}.sock']
    if scheme in ['tcp','ws']:args+=['--insecure-no-auth']
    log=(T/'evidence'/f'encryption-{scheme}-server.log').open('w')
    server=subprocess.Popen(args,env=env,stdout=subprocess.PIPE,stderr=log,text=True)
    reader=None
    try:
        while True:
            line=server.stdout.readline()
            if not line:raise RuntimeError(f'server stopped {server.poll()}')
            log.write(line)
            if ' pty=' in line:
                url=line.split(' pty=',1)[0];break
        def drain():
            for line in server.stdout:log.write(line)
        reader=threading.Thread(target=drain,daemon=True);reader.start()
        print(json.dumps({'phase':'start','scheme':scheme,'url':url}),flush=True)
        with (T/'evidence'/f'encryption-{scheme}.jsonl').open('w') as out:
            process=subprocess.Popen([str(CLIENT),url,str(T),str(server.pid),str(config)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            for line in process.stdout:
                out.write(line);out.flush();row=json.loads(line);row['scheme']=scheme;results.append(row);print(json.dumps(row),flush=True)
            error=process.stderr.read();code=process.wait()
            if code:raise RuntimeError(f'{scheme}: client exit {code}: {error}')
    finally:
        server.send_signal(signal.SIGINT)
        try:server.wait(timeout=5)
        except subprocess.TimeoutExpired:server.kill();server.wait()
        if reader is not None:reader.join(timeout=2)
        log.close()
    time.sleep(0.5)
report={'environment':{'platform':platform.platform(),'cpu':next((l.split(':',1)[1].strip() for l in Path('/proc/cpuinfo').read_text().splitlines() if l.startswith('model name')),None),'cpuClockTicks':os.sysconf('SC_CLK_TCK'),'mode':'release; loopback; independent server and SDK processes; no terminal rendering','ptyBytes':131072000,'fileAndForwardBytes':268435456,'rounds':'one warmup and three measured rounds; integrity verified'},'samples':results}
(T/'evidence/encryption-results.json').write_text(json.dumps(report,indent=2)+'\n')
