"""Single-instance lock: two `watch` daemons must never own the same tmux
session (that produced duplicate panes for every live session — see
acquire_lock's docstring)."""

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hermon import acquire_lock


class TestAcquireLock(unittest.TestCase):
    def test_second_acquire_for_same_session_fails(self):
        name = f"test-{os.getpid()}"
        lock1, holder1 = acquire_lock(name)
        self.addCleanup(lock1.close)
        self.assertIsNotNone(lock1)
        self.assertIsNone(holder1)

        lock2, holder2 = acquire_lock(name)
        self.assertIsNone(lock2)
        self.assertEqual(holder2, str(os.getpid()))

    def test_different_sessions_do_not_conflict(self):
        name_a = f"test-a-{os.getpid()}"
        name_b = f"test-b-{os.getpid()}"
        lock_a, _ = acquire_lock(name_a)
        lock_b, _ = acquire_lock(name_b)
        self.addCleanup(lock_a.close)
        self.addCleanup(lock_b.close)
        self.assertIsNotNone(lock_a)
        self.assertIsNotNone(lock_b)

    def test_lock_released_on_close_allows_reacquire(self):
        name = f"test-reacquire-{os.getpid()}"
        lock1, _ = acquire_lock(name)
        self.assertIsNotNone(lock1)
        lock1.close()  # flock releases when the fd closes

        lock2, holder2 = acquire_lock(name)
        self.addCleanup(lock2.close)
        self.assertIsNotNone(lock2)
        self.assertIsNone(holder2)


if __name__ == "__main__":
    unittest.main()
