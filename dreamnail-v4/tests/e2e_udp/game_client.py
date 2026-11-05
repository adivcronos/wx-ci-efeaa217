import socket,struct,os,sys,time,threading
DUR=float(sys.argv[1]); RATE=int(sys.argv[2])
# resolve via tun2proxy's virtual DNS (any :53 query is answered inside the TUN)
q=os.urandom(2)+b"\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00"+b"\x0e66-226-144-136\x03nip\x02io\x00"+b"\x00\x01\x00\x01"
d=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); d.settimeout(3); d.sendto(q,("1.1.1.1",53)); r=d.recv(512)
ip=socket.inet_ntoa(r[-4:]); print("  game server name resolved to (virtual):",ip)
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.connect((ip,40000)); s.settimeout(0.2)
st={"sent":0,"recv":0,"maxgap":0.0}; stop=time.time()+DUR; last=[None]; gaps=[]
def rx():
    while time.time()<stop+3:
        try: s.recv(4096)
        except socket.timeout: continue
        except OSError: continue
        now=time.time()
        if last[0] and now-last[0]>1: gaps.append(round(now-last[0],1))
        if last[0]: st["maxgap"]=max(st["maxgap"],now-last[0])
        last[0]=now; st["recv"]+=1
t=threading.Thread(target=rx); t.start(); seq=0
while time.time()<stop:
    try: s.send(struct.pack(">Q",seq)+os.urandom(92)); st["sent"]+=1
    except OSError: pass
    seq+=1; time.sleep(1/RATE)
t.join()
print(f"  sent {st['sent']}  got back {st['recv']} of {3*st['sent']} expected ({100*st['recv']/max(1,3*st['sent']):.1f}%)  longest silence {st['maxgap']:.2f}s  silences>1s: {gaps[:10]}")
