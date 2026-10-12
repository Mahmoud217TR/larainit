"""Engine-free harness checks: python3 tests/support/test_pty_check.py."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import tempfile
import os
import sys

temporary_parent = os.environ.get('TMPDIR') or ('/tmp/opencode' if Path('/tmp/opencode').is_dir() else None)

spec = importlib.util.spec_from_file_location('pty_check', Path(__file__).with_name('pty-check.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

cid = 'a' * 64
for engine in ('docker', 'podman'):
    with patch.object(module.subprocess, 'run') as run:
        try:
            module.verify_removed(engine, 'guessed-name')
        except ValueError:
            pass
        else:
            raise AssertionError('invalid ID authorized an engine call')
        run.assert_not_called()

with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1, stderr=b'')) as run:
    module.verify_removed('podman', cid)
    assert run.call_args.args[0] == ['podman', 'container', 'exists', '--', cid]

with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1, stderr=b'No such container: test-owned')) as run:
    module.verify_removed('docker', cid)
    assert run.call_args.args[0] == ['docker', 'container', 'inspect', '--', cid]

with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1, stderr=b'permission denied')) as run:
    try:
        module.verify_removed('docker', cid)
    except AssertionError:
        pass
    else:
        raise AssertionError('access failure counted as disappearance')
    assert run.call_count == 1

with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=125, stderr=b'storage unavailable')) as run:
    try:
        module.verify_removed('podman', cid)
    except AssertionError:
        pass
    else:
        raise AssertionError('storage failure counted as disappearance')
    assert run.call_count == 1

for engine in ('docker', 'podman'):
    with patch.object(module.subprocess, 'run', side_effect=[SimpleNamespace(returncode=0, stderr=b''), SimpleNamespace(returncode=0)]) as run:
        try:
            module.verify_removed(engine, cid)
        except AssertionError:
            pass
        else:
            raise AssertionError('leaked container counted as success')
        expected = ['docker', 'rm', '--force', '--volumes', '--', cid] if engine == 'docker' else ['podman', 'rm', '--force', '--time', '0', '--', cid]
        assert run.call_args.args[0] == expected

with patch.object(module.os, 'kill') as kill, patch.object(module.os, 'waitpid', return_value=(123, 0)) as wait:
    module.stop_wrapper(123)
    kill.assert_called_once_with(123, module.signal.SIGTERM)
    wait.assert_called_once_with(123, module.os.WNOHANG)

with patch.object(module.os, 'kill') as kill, patch.object(module.os, 'waitpid', side_effect=[(0, 0), (123, 0)]), patch.object(module.time, 'monotonic', side_effect=[0, 19, 19]):
    module.stop_wrapper(123)
    assert [call.args for call in kill.call_args_list] == [(123, module.signal.SIGTERM), (123, module.signal.SIGKILL)]

with patch.object(module.os, 'kill') as kill, patch.object(module.os, 'waitpid', return_value=(0, 0)), patch.object(module.time, 'monotonic', side_effect=[0, 19, 19, 22]):
    try:
        module.stop_wrapper(123)
    except TimeoutError:
        pass
    else:
        raise AssertionError('unreaped wrapper counted as bounded cleanup')
    assert [call.args[0] for call in kill.call_args_list] == [123, 123]

with tempfile.TemporaryDirectory(prefix='larainit-cid-safety-', dir=temporary_parent) as root:
    path = Path(root) / 'cid'
    for data in [cid.encode(), (cid + '\n').encode()]:
        path.write_bytes(data)
        assert module.captured_id(path) == cid
    for data in [b'', b'a' * 63, b'a' * 65, b'A' * 64, b'a' * 64 + b'\n\n', b'a' * 64 + b'\x00', b'\xff' * 64, b'a' * 64 + b'\n' + b'x' * 100000]:
        path.write_bytes(data)
        try:
            module.captured_id(path)
        except ValueError:
            pass
        else:
            raise AssertionError('malformed CID authorized a cleanup identity')
    path.unlink()
    other = Path(root) / 'unrelated'
    other.write_text(cid)
    path.symlink_to(other)
    try:
        module.captured_id(path)
    except OSError:
        pass
    else:
        raise AssertionError('symlink CID accepted')
    path.unlink()
    os.mkfifo(path)
    try:
        module.captured_id(path)
    except ValueError:
        pass
    else:
        raise AssertionError('FIFO CID accepted')
    assert other.read_text() == cid

with tempfile.TemporaryDirectory(prefix='larainit-pty-safety-', dir=temporary_parent) as root:
    module.root = Path(root)
    module.binary, module.image, module.engine, module.explicit_engine = '/unused/larainit', 'test:tag', 'podman', True
    def invalid_cid_ready(*args):
        workspace = module.root / 'invalid-cid'
        (workspace / 'ready').write_bytes(b'ready')
        scratch = workspace / 'runtime' / 'larainit-test'
        scratch.mkdir()
        (scratch / 'cid').write_text('guessed-name')
        return [], [], []
    with patch.object(module.pty, 'openpty', return_value=(10, 11)), patch.object(module.fcntl, 'ioctl'), patch.object(module.termios, 'tcgetattr', return_value=[]), patch.object(module.os, 'fork', return_value=123), patch.object(module.os, 'set_blocking'), patch.object(module.os, 'close') as close, patch.object(module.select, 'select', side_effect=invalid_cid_ready), patch.object(module, 'stop_wrapper') as stop, patch.object(module.subprocess, 'run') as engine_run:
        try:
            module.run('invalid-cid', [], capture_id=True)
        except ValueError:
            pass
        else:
            raise AssertionError('invalid CID counted as a completed PTY run')
        stop.assert_called_once_with(123)
        assert [call.args for call in close.call_args_list][-2:] == [(10,), (11,)]
        engine_run.assert_not_called()
        assert (module.root / 'invalid-cid' / 'terminal.log').is_file()

with tempfile.TemporaryDirectory(prefix='larainit-foreground-safety-', dir=temporary_parent) as root:
    module.root, module.results = Path(root), []
    module.engine, module.explicit_engine, module.image = 'docker', False, 'unused:tag'
    client = Path(root) / 'fake-client'
    client.write_text(f'#!{sys.executable}\n' + '''import os, signal, sys, termios
from pathlib import Path
Path('wrapper.pid').write_text(str(os.getpid()))
if sys.argv[-1] == 'bad-termios':
    attributes = termios.tcgetattr(0)
    attributes[3] ^= termios.ECHO
    termios.tcsetattr(0, termios.TCSANOW, attributes)
    os._exit(42)
if sys.argv[-1] == 'interrupt':
    signal.signal(signal.SIGINT, lambda *_: sys.exit(42))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(42))
    scratch = Path(os.environ['TMPDIR']) / 'larainit-fake'
    scratch.mkdir()
    (scratch / 'cid').write_text('a' * 64)
    Path('ready').write_bytes(b'ready')
    while True:
        signal.pause()
signal.signal(signal.SIGTTOU, signal.SIG_IGN)
before = os.tcgetpgrp(0)
read, write = os.pipe()
worker = os.fork()
if worker == 0:
    os.close(write)
    os.setpgid(0, 0)
    os.read(read, 1)
    os._exit(0)
os.close(read)
os.setpgid(worker, worker)
os.tcsetpgrp(0, worker)
os.write(write, b'go')
os.close(write)
os.waitpid(worker, 0)
if sys.argv[-1] == 'restore':
    os.tcsetpgrp(0, before)
os._exit(42)
''')
    client.chmod(0o700)
    module.binary = str(client)
    result, _ = module.run('restored-foreground', ['restore'])
    assert result['status'] == 42 and result['foreground_restored'] is True
    try:
        module.run('broken-foreground', ['leave-assigned'])
    except AssertionError as error:
        assert 'foreground' in str(error)
    else:
        raise AssertionError('foreground assignment was not audited')
    try:
        module.run('broken-termios', ['bad-termios'])
    except AssertionError as error:
        assert 'terminal not restored' in str(error)
    else:
        raise AssertionError('termios mutation was not audited')
    result, _ = module.run('fake-ctrl-c', ['interrupt'], interrupt=True)
    assert result['status'] == 42 and result['foreground_restored'] and result['terminal_restored']
    assert result['container_id'] == cid
    real_select = module.select.select
    def deadline_after_ready(*args):
        if (module.root / 'fake-timeout' / 'ready').exists():
            raise TimeoutError('injected test deadline')
        return real_select(*args)
    with patch.object(module.select, 'select', side_effect=deadline_after_ready):
        try:
            module.run('fake-timeout', ['interrupt'])
        except TimeoutError as error:
            assert str(error) == 'injected test deadline'
        else:
            raise AssertionError('injected timeout did not fail')
    wrapper_pid = (module.root / 'fake-timeout' / 'wrapper.pid').read_text()
    assert not Path(f'/proc/{wrapper_pid}').exists(), 'test wrapper survived supervisor TERM forwarding/reap'
    assert (module.root / 'fake-timeout' / 'foreground.json').is_file()
    client.chmod(0o600)
    result, _ = module.run('unexecutable-client', [])
    assert result['status'] == 126 and result['foreground_restored'] and result['terminal_restored']

print('PTY harness safety checks passed (no engine invoked)')
