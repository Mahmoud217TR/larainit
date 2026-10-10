"""Real Docker terminal checks; developer-only, Python standard library.

Run: python3 tests/support/pty-check.py /absolute/native/larainit PREPARED_IMAGE [--installer]
Logs and isolated workspaces are retained in the printed temporary directory.
"""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time

binary = str(Path(sys.argv[1]).resolve(strict=True))
image = sys.argv[2]
root = Path(tempfile.mkdtemp(prefix='larainit-pty-', dir=os.environ.get('TMPDIR', '/tmp')))
results = []


def run(name, arguments, input_bytes=None, redirect=False, closed=False, interrupt=False, prompts=()):
    workspace = root / name
    workspace.mkdir()
    env = os.environ.copy()
    env.update(XDG_CONFIG_HOME=str(workspace / 'config'), XDG_CACHE_HOME=str(root / 'cache'), TERM='xterm-256color')
    command = [binary, '--image', image, *arguments]
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
        if redirect:
            fd = os.open(workspace / 'stdout', os.O_CREAT | os.O_WRONLY, 0o600)
            os.dup2(fd, 1)
            os.close(fd)
        if closed:
            os.close(0)
        os.chdir(workspace)
        os.execve(binary, command, env)
    os.set_blocking(master, False)
    output = bytearray()
    sent = False
    start = time.monotonic()
    status = None
    answered = []
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
        if not sent and interrupt and (workspace / 'ready').exists():
            os.write(master, b'\x03')
            sent = True
        elif not sent and input_bytes is not None and time.monotonic() - start > 0.5:
            os.write(master, input_bytes)
            sent = True
        exited, wait = os.waitpid(pid, os.WNOHANG)
        if exited:
            status = os.waitstatus_to_exitcode(wait)
        if time.monotonic() - start > (600 if prompts else 30):
            os.kill(pid, signal.SIGTERM)
            raise TimeoutError(f'{name}: test deadline; requested wrapper cleanup; inspect invocation manually if needed')
    after = termios.tcgetattr(slave)
    os.close(master)
    os.close(slave)
    (workspace / 'terminal.log').write_bytes(output)
    result = {'name': name, 'command': command, 'status': status, 'terminal_restored': before == after}
    if prompts:
        result['answered_prompts'] = answered
        assert answered == list(prompts), 'actual installer prompts were not answered'
    if redirect:
        result['stdout'] = (workspace / 'stdout').read_text()
    results.append(result)
    (root / 'results.json').write_text(json.dumps(results, indent=2))
    assert before == after, f'{name}: terminal not restored'
    return result, bytes(output)


result, _ = run('redirected-output', ['php', '-r', 'echo fgets(STDIN); fwrite(STDERR,"separate-error\\n");'], b'terminal input\n', redirect=True)
assert result['status'] == 0 and result['stdout'] == 'terminal input\n', result
result, _ = run('closed-input', ['php', '-r', 'echo strlen(stream_get_contents(STDIN));'], closed=True, redirect=True)
assert result['status'] == 0 and result['stdout'] == '0', result
result, output = run('ctrl-c', ['bash', '-c', "trap 'exit 42' INT TERM; touch ready; while :; do sleep 0.1; done"], interrupt=True)
assert result['status'] == 42, result
result, output = run('bare-bash', [], b'printf "native-shell-ok\\n"; exit 0\n')
assert result['status'] == 0 and b'native-shell-ok' in output, result
if '--installer' in sys.argv[3:]:
    result, output = run('installer', ['new', 'my-app', '--database=sqlite', '--phpunit', '--no-node', '--no-boost'],
                         prompts=('Do you want to use a starter kit?', 'Which frontend stack do you want to build on?'))
    assert result['status'] == 0, result
    project = root / 'installer' / 'my-app'
    assert (project / 'artisan').is_file()
    assert (project / '.env').stat().st_uid == os.getuid()
    with (project / '.env').open('a') as file:
        file.write('\n# host edit after real prompt verification\n')
print(json.dumps(results, indent=2))
print('Retained PTY evidence:', root)
