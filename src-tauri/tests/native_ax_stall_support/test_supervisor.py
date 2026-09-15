"""Pure state/protocol tests: no child launch, GUI, signals, or libproc calls."""
import io
import os
import signal
import time
import unittest
from unittest.mock import patch
import supervisor as s


class ProtocolTests(unittest.TestCase):
    def test_transition_matrix(self):
        accepted = {('RUNNING', 'STOP'): 'STOPPED', ('STOPPED', 'CONT'): 'RUNNING',
                    ('STOPPED', 'CHECK'): 'STOPPED'}
        for state in ('RUNNING', 'STOPPED', 'EXITED'):
            for command in ('STOP', 'CONT', 'CHECK', 'OTHER', 'DONE'):
                if (state, command) in accepted:
                    self.assertEqual(s.transition(state, command), accepted[state, command])
                else:
                    with self.assertRaises(ValueError):
                        s.transition(state, command)

    def read_pipe(self, payload, close=True):
        read, write = os.pipe()
        try:
            os.write(write, payload)
            if close:
                os.close(write)
                write = None
            return s.line(read, time.monotonic()+.02)
        finally:
            os.close(read)
            if write is not None:
                os.close(write)

    def test_framing(self):
        self.assertEqual(self.read_pipe(b'STOP\n'), 'STOP')
        for data in (b'STOP', b'x'*161, b'\xff\n'):
            with self.assertRaises((ValueError, UnicodeError)):
                self.read_pipe(data)
        with self.assertRaises(TimeoutError):
            self.read_pipe(b'S', close=False)

    def test_identity_mismatch_never_signals(self):
        owner = object.__new__(s.Owner)
        owner.reaped, owner.identity = False, ('original',)
        owner.read_identity = lambda: ('replacement',)
        with patch.object(s.os, 'kill') as kill:
            with self.assertRaises(AssertionError):
                owner.send(signal.SIGSTOP)
            kill.assert_not_called()
        owner.reaped = True
        with self.assertRaises(AssertionError):
            owner.send(signal.SIGCONT)

    def test_cleanup_cont_first_reap_once(self):
        owner = object.__new__(s.Owner)
        owner.reaped = False
        owner.child = type('FakeChild', (), {'stdin': io.BytesIO()})()
        events = []
        owner.send = lambda sig: events.append(sig)
        def observe():
            if events:
                events.append('reap')
                owner.reaped = True
        owner.observe = observe
        owner.cleanup()
        owner.cleanup()
        self.assertEqual(events, [signal.SIGCONT, 'reap'])
        self.assertEqual(owner.child.stdin.getvalue(), b'Q')

    def test_stop_requires_wait_ack(self):
        owner = object.__new__(s.Owner)
        owner.state, owner.reaped, owner.identity = 'RUNNING', False, (1,)
        owner.read_identity = lambda: (1,)
        events = []
        owner.send = lambda sig: events.append(sig)
        observations = iter([(0, 0), (99, (signal.SIGSTOP << 8) | 0x7f)])
        owner.observe = lambda: next(observations)
        with patch.object(s.time, 'sleep'):
            owner.command('STOP')
        self.assertEqual(events, [signal.SIGSTOP])
        self.assertEqual(owner.state, 'STOPPED')
        owner.state = 'RUNNING'
        owner.observe = lambda: (0, 0)
        ticks = iter(range(10))
        with patch.object(s.time, 'monotonic', side_effect=lambda: next(ticks)):
            with self.assertRaises(TimeoutError):
                owner.command('STOP')
        self.assertEqual(owner.state, 'RUNNING')

    def test_terminal_observation_disarms_once(self):
        owner = object.__new__(s.Owner)
        owner.reaped = False
        owner.child = type('FakeChild', (), {'pid': 99, 'returncode': None})()
        with patch.object(s.os, 'waitpid', return_value=(99, 0)) as wait:
            owner.observe()
            owner.cleanup()
            self.assertEqual(wait.call_count, 1)
        self.assertTrue(owner.reaped)
        self.assertEqual(owner.state, 'EXITED')
        self.assertEqual(owner.child.returncode, 0)

    def test_cleanup_escalation_is_bounded(self):
        owner = object.__new__(s.Owner)
        owner.reaped = False
        owner.child = type('FakeChild', (), {'stdin': io.BytesIO()})()
        events = []
        owner.send = lambda sig: events.append(sig)
        owner.observe = lambda: None
        ticks = iter(range(100))
        with patch.object(s.time, 'monotonic', side_effect=lambda: next(ticks)):
            with self.assertRaises(RuntimeError):
                owner.cleanup()
        self.assertEqual(events, [signal.SIGCONT, signal.SIGTERM, signal.SIGKILL])

    def test_done_rejects_preexisting_nonzero_or_signal_exit(self):
        for status in (1 << 8, signal.SIGKILL):
            owner = object.__new__(s.Owner)
            owner.reaped = False
            owner.child = type('FakeChild', (), {'pid': 99, 'returncode': None})()
            with patch.object(s.os, 'waitpid', return_value=(99, status)), patch.object(s.os, 'kill') as kill:
                with self.assertRaisesRegex(RuntimeError, 'unexpected_fixture_exit'):
                    owner.cleanup(require_graceful=True)
                kill.assert_not_called()
                self.assertTrue(owner.reaped)

    def test_done_accepts_only_requested_zero_exit_without_escalation(self):
        for returncode in (0, 1, -signal.SIGKILL):
            owner = object.__new__(s.Owner)
            owner.reaped = False
            owner.child = type('FakeChild', (), {'stdin': io.BytesIO(), 'returncode': returncode})()
            owner.send = lambda sig: self.assertEqual(sig, signal.SIGCONT)
            def observe():
                if owner.child.stdin.getvalue() == b'Q':
                    owner.reaped = True
            owner.observe = observe
            if returncode == 0:
                owner.cleanup(require_graceful=True)
            else:
                with self.assertRaisesRegex(RuntimeError, 'unexpected_fixture_exit'):
                    owner.cleanup(require_graceful=True)
            self.assertTrue(owner.reaped)

    def test_identity_lookup_exit_race_reaps_without_signalling(self):
        owner = object.__new__(s.Owner)
        owner.reaped, owner.identity = False, ('owned',)
        owner.child = type('FakeChild', (), {'stdin': io.BytesIO(), 'returncode': None})()
        calls = []
        def observe():
            calls.append('observe')
            if len(calls) == 2:
                owner.reaped = True
                owner.child.returncode = 0
        owner.observe = observe
        def lost_identity():
            raise AssertionError('already exited')
        owner.read_identity = lost_identity
        with patch.object(s.os, 'kill') as kill:
            owner.cleanup()
            kill.assert_not_called()
        self.assertTrue(owner.reaped)
        self.assertEqual(len(calls), 2)
        self.assertTrue(owner.child.stdin.closed)


if __name__ == '__main__':
    unittest.main()
