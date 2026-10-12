"""Real engine terminal checks; developer-only, Python standard library.

Run: python3 tests/support/pty-check.py /absolute/native/larainit PREPARED_IMAGE [--installer] [--engine docker|podman]
Logs and isolated workspaces are retained in the printed temporary directory.
"""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import stat
import subprocess
import struct
import tempfile
import termios
import time

def captured_id(path):
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC), 'rb') as file:
        if not stat.S_ISREG(os.fstat(file.fileno()).st_mode):
            raise ValueError('CID must be a regular file; no guessed cleanup')
        identity = file.read(66).decode('ascii').removesuffix('\n')
    if re.fullmatch(r'[0-9a-f]{64}', identity) is None:
        raise ValueError('invalid captured container ID; no guessed cleanup')
    return identity


def verify_removed(engine, container_id):
    if re.fullmatch(r'[0-9a-f]{64}', container_id) is None:
        raise ValueError('full captured container ID required; no engine request authorized')
    if engine == 'podman':
        inspection = subprocess.run(['podman', 'container', 'exists', '--', container_id], capture_output=True, timeout=5)
        absent = inspection.returncode == 1
        remove = ['podman', 'rm', '--force', '--time', '0', '--', container_id]
    elif engine == 'docker':
        inspection = subprocess.run(['docker', 'container', 'inspect', '--', container_id], capture_output=True, timeout=5)
        absent = inspection.returncode != 0 and b'No such' in inspection.stderr
        remove = ['docker', 'rm', '--force', '--volumes', '--', container_id]
    else:
        raise ValueError('unsupported test engine')
    if inspection.returncode == 0:
        subprocess.run(remove, check=True, capture_output=True, timeout=10)
    assert absent, f'container disappearance not proven ({engine} status {inspection.returncode}); leakage or engine access failure is not a pass'
    return inspection.returncode


def stop_wrapper(pid):
    # Only the unreaped wrapper PID returned by our fork, never an engine/container guess.
    for requested, limit in [(signal.SIGTERM, 18), (signal.SIGKILL, 2)]:
        try:
            os.kill(pid, requested)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + limit
        while True:
            try:
                exited, _ = os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                return
            if exited:
                return
            if time.monotonic() >= deadline:
                break
            time.sleep(0.01)
    raise TimeoutError('test-owned wrapper could not be reaped; inspect invocation manually')


def run(name, arguments, input_bytes=None, redirect=False, closed=False, interrupt=False, prompts=(), capture_id=False):
    capture_id = capture_id or interrupt
    workspace = root / name
    workspace.mkdir(mode=0o700)
    runtime = workspace / 'runtime'
    runtime.mkdir(mode=0o700)
    env = os.environ.copy()
    env.update(XDG_CACHE_HOME=str(root / 'cache'), TMPDIR=str(runtime), TERM='xterm-256color')
    if engine == 'docker':
        env['XDG_CONFIG_HOME'] = str(workspace / 'config')
    command = [binary, *(['--engine', engine] if explicit_engine else []), '--image', image, *arguments]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
    before = termios.tcgetattr(slave)
    pid = os.fork()
    if pid == 0:
        os.close(master)
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        for fd in range(3):
            os.dup2(slave, fd)
        if slave > 2:
            os.close(slave)
        # Keep the controlling session alive to inspect restoration before terminal teardown.
        foreground = os.tcgetpgrp(0)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        wrapper = os.fork()
        if wrapper == 0:
            signal.signal(signal.SIGINT, signal.SIG_DFL)
            signal.signal(signal.SIGTERM, signal.SIG_DFL)
            if redirect:
                fd = os.open(workspace / 'stdout', os.O_CREAT | os.O_WRONLY, 0o600)
                os.dup2(fd, 1)
                os.close(fd)
            if closed:
                os.close(0)
            os.chdir(workspace)
            try:
                os.execve(binary, command, env)
            except OSError:
                os.write(2, b'PTY wrapper exec failed\n')
                os._exit(126)
        def forward(requested, _):
            try:
                os.kill(wrapper, requested)
            except ProcessLookupError:
                pass
        signal.signal(signal.SIGTERM, forward)
        while True:
            # Disable forwarding before releasing the wrapper PID for reuse.
            signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM})
            exited, wait = os.waitpid(wrapper, os.WNOHANG)
            if exited:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
            signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})
            if exited:
                break
            time.sleep(0.01)
        restored = os.tcgetpgrp(0)
        (workspace / 'foreground.json').write_text(json.dumps({'before': foreground, 'after': restored}))
        code = os.waitstatus_to_exitcode(wait)
        os._exit(code if code >= 0 else 128 - code)
    os.set_blocking(master, False)
    output = bytearray()
    sent = False
    start = time.monotonic()
    status = None
    answered = []
    container_id = None
    try:
        while status is None:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            if prompts:
                text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output.decode(errors='replace'))
                for prompt in prompts:
                    if prompt in text and prompt not in answered:
                        os.write(master, b'\r')
                        answered.append(prompt)
            if capture_id and container_id is None and (workspace / 'ready').exists():
                identities = list(runtime.glob('larainit-*/cid'))
                if len(identities) == 1:
                    try:
                        container_id = captured_id(identities[0])
                    except FileNotFoundError:
                        pass  # Auto-removal may win the glob/open race; never substitute a name.
            if not sent and interrupt and container_id is not None:
                os.write(master, b'\x03')
                sent = True
            elif not sent and input_bytes is not None and time.monotonic() - start > 0.5 and (not capture_id or container_id is not None):
                os.write(master, input_bytes)
                sent = True
            exited, wait = os.waitpid(pid, os.WNOHANG)
            if exited:
                status = os.waitstatus_to_exitcode(wait)
            elif time.monotonic() - start > (600 if prompts else 30):
                raise TimeoutError(f'{name}: test deadline; inspect invocation manually if needed')
        after = termios.tcgetattr(slave)
    finally:
        try:
            if status is None:
                stop_wrapper(pid)
        finally:
            os.close(master)
            os.close(slave)
            (workspace / 'terminal.log').write_bytes(output)
    result = {'name': name, 'command': command, 'status': status, 'terminal_restored': before == after}
    foreground = json.loads((workspace / 'foreground.json').read_text())
    result['foreground_restored'] = foreground['before'] == foreground['after']
    if container_id:
        result['container_id'] = container_id
    if prompts:
        result['answered_prompts'] = answered
        assert answered == list(prompts), 'actual installer prompts were not answered'
    if redirect:
        result['stdout'] = (workspace / 'stdout').read_text()
    results.append(result)
    (root / 'results.json').write_text(json.dumps(results, indent=2))
    assert before == after, f'{name}: terminal not restored'
    assert result['foreground_restored'], f'{name}: terminal foreground process group not restored'
    return result, bytes(output)


def main():
    global binary, image, root, results, engine, explicit_engine
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary')
    parser.add_argument('image')
    parser.add_argument('--installer', action='store_true')
    parser.add_argument('--engine', choices=('docker', 'podman'), help='requires an independently certified real-engine fixture')
    options = parser.parse_args()
    binary = str(Path(options.binary).resolve(strict=True))
    image = options.image
    engine = options.engine or 'docker'
    explicit_engine = options.engine is not None
    root = Path(tempfile.mkdtemp(prefix='larainit-pty-', dir=os.environ.get('TMPDIR', '/tmp')))
    results = []
    result, _ = run('redirected-output', ['php', '-r', 'echo fgets(STDIN); fwrite(STDERR,"separate-error\\n");'], b'terminal input\n', redirect=True)
    assert result['status'] == 0 and result['stdout'] == 'terminal input\n', result
    result, _ = run('closed-input', ['php', '-r', 'echo strlen(stream_get_contents(STDIN));'], closed=True, redirect=True)
    assert result['status'] == 0 and result['stdout'] == '0', result
    result, output = run('ctrl-c', ['bash', '-c', "trap 'exit 42' INT TERM; touch ready; while :; do sleep 0.1; done"], interrupt=True, capture_id=True)
    assert result['status'] == 42, result
    result['disappearance_status'] = verify_removed(engine, result['container_id'])
    result, output = run('bare-bash', [], b'printf "native-shell-ok\\n"; exit 0\n')
    assert result['status'] == 0 and b'native-shell-ok' in output, result
    result, output = run('detached-shell', ['bash', '-c', 'touch ready; while :; do sleep 0.1; done'], b'\x10\x11', capture_id=True)
    assert result['status'] == 0, result
    result['disappearance_status'] = verify_removed(engine, result['container_id'])
    if options.installer:
        result, output = run('installer', ['new', 'my-app', '--database=sqlite', '--phpunit', '--no-node', '--no-boost'],
                             prompts=('Do you want to use a starter kit?', 'Which frontend stack do you want to build on?'))
        assert result['status'] == 0, result
        project = root / 'installer' / 'my-app'
        assert (project / 'artisan').is_file()
        assert (project / '.env').stat().st_uid == os.getuid()
        with (project / '.env').open('a') as file:
            file.write('\n# host edit after real prompt verification\n')
    (root / 'results.json').write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))
    print('Retained PTY evidence:', root)


if __name__ == '__main__':
    main()
