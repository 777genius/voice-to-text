"""Mac-only sole signal authority. Protocol stdout contains fixed tokens/numbers only."""
import ctypes as C
import os
import json
from pathlib import Path
import plistlib
import secrets
import select
import signal
import subprocess
import sys
import time


class BSD(C.Structure):
    _fields_ = [(n, C.c_uint32) for n in (
        'flags status xstatus pid ppid uid gid ruid rgid svuid svgid reserved'.split())] + [
        ('comm', C.c_char * 16), ('name', C.c_char * 32)] + [
        (n, C.c_uint32) for n in 'nfiles pgid jobc tdev tpgid nice'.split()] + [
        ('start_sec', C.c_uint64), ('start_usec', C.c_uint64)]


def transition(state, command):
    allowed = {('RUNNING', 'STOP'): 'STOPPED', ('STOPPED', 'CHECK'): 'STOPPED',
               ('STOPPED', 'CONT'): 'RUNNING'}
    if (state, command) not in allowed:
        raise ValueError('protocol')
    return allowed[state, command]


def line(fd, deadline):
    data = bytearray()
    while time.monotonic() < deadline:
        if select.select([fd], [], [], min(.05, max(0, deadline-time.monotonic())))[0]:
            b = os.read(fd, 1)
            if not b:
                raise ValueError('eof')
            if b == b'\n':
                return data.decode('ascii')
            data += b
            if len(data) > 160:
                raise ValueError('overflow')
    raise TimeoutError('watchdog')


class Owner:
    def __init__(self, child, exe):
        self.child, self.exe, self.reaped = child, str(exe.resolve()), False
        self.lib = C.CDLL('/usr/lib/libproc.dylib')
        self.lib.proc_pidinfo.argtypes = [C.c_int, C.c_int, C.c_uint64, C.c_void_p, C.c_int]
        self.lib.proc_pidpath.argtypes = [C.c_int, C.c_void_p, C.c_uint32]
        self.identity = self.read_identity()
        self.state = 'RUNNING'

    def read_identity(self):
        info, path = BSD(), C.create_string_buffer(4096)
        assert self.lib.proc_pidinfo(self.child.pid, 3, 0, C.byref(info), C.sizeof(info)) == C.sizeof(info)
        assert self.lib.proc_pidpath(self.child.pid, path, len(path)) > 0
        assert info.pid == self.child.pid > 0 and info.ppid == os.getpid()
        assert info.uid == os.getuid() and os.path.realpath(os.fsdecode(path.value)) == self.exe
        return (info.pid, info.uid, info.start_sec, info.start_usec, self.exe)

    def send(self, sig):
        assert not self.reaped and self.read_identity() == self.identity
        os.kill(self.child.pid, sig)  # unreaped direct child: PID cannot be reused

    def observe(self):
        assert not self.reaped
        pid, status = os.waitpid(self.child.pid, os.WNOHANG | os.WUNTRACED)
        if pid and (os.WIFEXITED(status) or os.WIFSIGNALED(status)):
            self.reaped = True
            self.child.returncode = os.waitstatus_to_exitcode(status)
            self.state = 'EXITED'
        return pid, status

    def command(self, command):
        next_state = transition(self.state, command)
        assert self.read_identity() == self.identity
        if command == 'STOP':
            self.send(signal.SIGSTOP)
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline:
                pid, status = self.observe()
                if self.reaped:
                    raise ValueError('exit')
                if pid and os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGSTOP:
                    self.state = next_state
                    return
                time.sleep(.005)
            raise TimeoutError('stop')
        if command == 'CHECK':
            info = BSD()
            assert self.lib.proc_pidinfo(self.child.pid, 3, 0, C.byref(info), C.sizeof(info)) == C.sizeof(info)
            assert info.status == 4  # Darwin SSTOP, not merely successful kill()
        if command == 'CONT':
            self.send(signal.SIGCONT)
        self.state = next_state

    def send_or_reap(self, sig):
        try:
            self.send(sig)
            return True
        except (AssertionError, OSError):
            # The direct child may have exited between observe and libproc.
            # Identity failure never authorizes another numeric-PID signal.
            try:
                self.child.stdin.close()
            except (BrokenPipeError, ValueError):
                pass
            deadline = time.monotonic() + .7
            while time.monotonic() < deadline:
                self.observe()
                if self.reaped:
                    return False
                time.sleep(.01)
            raise RuntimeError('identity_unresolved')

    def cleanup(self, require_graceful=False):
        def terminal(expected=False):
            if require_graceful and (not expected or self.child.returncode != 0):
                raise RuntimeError('unexpected_fixture_exit')
        if self.reaped:
            terminal()
            return
        self.observe()  # reap an independently exited child before any signal
        if self.reaped:
            terminal()
            return
        if not self.send_or_reap(signal.SIGCONT):
            terminal()
            return
        requested = False
        try:
            self.child.stdin.write(b'Q')
            self.child.stdin.flush()
            requested = True
        except BrokenPipeError:
            pass
        escalated = False
        for escalation in (signal.SIGTERM, signal.SIGKILL, None):
            deadline = time.monotonic() + .7
            while time.monotonic() < deadline:
                self.observe()
                if self.reaped:
                    terminal(requested and not escalated)
                    return
                time.sleep(.01)
            if escalation is not None:
                escalated = True
                if not self.send_or_reap(escalation):
                    terminal()
                    return
        raise RuntimeError('reap')


def main():
    assert sys.platform == 'darwin' and os.environ.get('VT_REAL_AX_SYNTHETIC') == '1'
    root = Path(sys.argv[1])
    assert root.is_absolute() and str(root).isascii() and root.parent == Path('/tmp')
    root.mkdir(mode=0o700)  # exclusively NEW; never accept an existing directory
    assert root.stat().st_mode & 0o777 == 0o700
    nonce = secrets.token_hex(16)
    bundle = 'org.voicetext.synthetic.ax.' + nonce
    contents = root / 'Fixture.app' / 'Contents'
    binary = contents / 'MacOS' / 'Fixture'
    binary.parent.mkdir(parents=True)
    with (contents / 'Info.plist').open('wb') as f:
        plistlib.dump(dict(CFBundleIdentifier=bundle, CFBundleExecutable='Fixture',
                          CFBundlePackageType='APPL', NSPrincipalClass='NSApplication'), f)
    subprocess.run(['/usr/bin/clang', '-fobjc-arc', '-fblocks', '-framework', 'Cocoa',
                    str(Path(__file__).with_name('fixture.m')), '-o', str(binary)],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=20)
    child = subprocess.Popen([str(binary), nonce], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        owner = Owner(child, binary)
    except BaseException:
        # Identity unavailable: no numeric-PID signal is authorized. Private stdin EOF
        # asks this direct fixture to exit; preserve failed qualification.
        child.stdin.close()
        child.wait(timeout=2)
        raise
    success = False
    try:
        assert line(child.stdout.fileno(), time.monotonic()+5) == f'READY {nonce} {bundle}'
        with (root / 'identity.json').open('x') as evidence:
            json.dump(dict(pid=owner.identity[0], uid=owner.identity[1],
                           start_sec=owner.identity[2], start_usec=owner.identity[3],
                           executable=owner.exe, nonce=nonce, bundle=bundle), evidence)
        print(f'READY {child.pid} {bundle}', flush=True)
        whole = time.monotonic()+25
        stop_deadline = whole
        while True:
            command = line(0, min(whole, stop_deadline))
            if command == 'DONE':
                success = True
                break
            owner.command(command)
            if command == 'STOP':
                stop_deadline = time.monotonic()+3
            elif command == 'CONT':
                stop_deadline = whole
            print(owner.state, flush=True)
    except BaseException:
        owner.cleanup()
        print('FAIL', flush=True)
        # Cleanup is already complete. Stay independent/alive until driver cleanup
        # acknowledgement or private-pipe EOF; never rearm signals or watchdogs.
        try:
            while line(0, time.monotonic()+3600) != 'DONE':
                pass
        except (ValueError, TimeoutError):
            pass
        raise
    else:
        owner.cleanup(require_graceful=True)
    print('CLEAN' if success else 'FAIL', flush=True)


if __name__ == '__main__':
    try:
        main()
    except BaseException:
        print('FAIL', flush=True)
        sys.exit(1)
