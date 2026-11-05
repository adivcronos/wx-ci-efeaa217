import socket,time,sys,collections,signal
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(("0.0.0.0",40000)); s.settimeout(1)
ports=collections.Counter(); end=time.time()+float(sys.argv[1]); n=0
def report(*a):
    print("packets",n,"| distinct client ports",len(ports),dict(ports.most_common(4)),flush=True); sys.exit(0)
signal.signal(signal.SIGTERM, report)
while time.time()<end:
    try: d,a=s.recvfrom(2048)
    except socket.timeout: continue
    ports[a[1]]+=1; n+=1
    for i in range(3): s.sendto(d[:8]+bytes([i])+b"G"*1191,a)
report()
