import sys,subprocess,time; sys.path.insert(0,"/home/cronoso5/dreamnail/admin")
# E2E game-UDP test. Usage: DN_ADMIN_PW=.. DN_TEST_USER=.. DN_TEST_PASS=.. python drive.py <dir-with-cli+e2e/> <tunhelper> <tag> <secs>
from storage import Storage; from ssh_admin import SSHAdmin
SP=sys.argv[1]; helper=sys.argv[2]; tag=sys.argv[3]; dur=sys.argv[4]
st=Storage("admin.enc","admin_salt.bin","x",{}); import os; st.unlock(os.environ["DN_ADMIN_PW"]); g=st.get
s=SSHAdmin(g("ssh_host"),22,"root",password=g("ssh_pass"))
sf=s._conn.open_sftp(); sf.put(f"{SP}/e2e/gamesrv.py","/root/gamesrv-test.py"); sf.close()
s.run("systemctl stop gamesrv-test 2>/dev/null; systemctl reset-failed gamesrv-test 2>/dev/null; systemd-run --unit=gamesrv-test python3 -u /root/gamesrv-test.py %d" % (int(dur)+60)); time.sleep(1.5); print("  game server up:", s.run("ss -ulnp | grep -c :40000")[1].strip())
time.sleep(1)
sh=open(f"{SP}/e2e/run.sh").read().replace("game_client.py 40 100", f"game_client.py {dur} 100")
open(f"{SP}/e2e/run_{tag}.sh","w").write(sh)
p=subprocess.run(["timeout",str(int(dur)+90),"pasta","--config-net","--","sh",f"{SP}/e2e/run_{tag}.sh",helper,f"{SP}/e2e/{tag}",SP],capture_output=True,text=True)
print("\n".join(l for l in p.stdout.splitlines() if not l.startswith(("default","10.","66.","0.0.0.0","128.")) ))
print("  server gateway:", s.run("journalctl -u dreamnail-udpgw --since '%d sec ago' -o cat --no-pager | grep -E 'flow 66.226' | sed 's/INFO //' | tr '\\n' ';'" % (int(dur)+40))[1].strip())
s.run("systemctl stop gamesrv-test; sleep 1"); print("  game server saw:", s.run("journalctl -u gamesrv-test --since '5 min ago' -o cat --no-pager | grep packets | tail -1; rm -f /root/gamesrv-test.py")[1].strip())
s.shutdown()
