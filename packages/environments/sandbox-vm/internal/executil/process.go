package executil

import (
	"errors"
	"os"
	"os/exec"
	"sync"
	"syscall"
	"time"
)

const DefaultCancelGracePeriod = 3 * time.Second

var CancelGracePeriod = DefaultCancelGracePeriod

// ConfigureCommandCancellation puts the command in its own process group and
// changes context cancellation to send SIGTERM first, then SIGKILL the whole
// group after CancelGracePeriod if it is still alive.
func ConfigureCommandCancellation(cmd *exec.Cmd) {
	if cmd == nil {
		return
	}

	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Setpgid = true
	cmd.WaitDelay = CancelGracePeriod

	var cancelOnce sync.Once
	cmd.Cancel = func() error {
		var cancelErr error
		cancelOnce.Do(func() {
			if cmd.Process == nil {
				cancelErr = os.ErrProcessDone
				return
			}

			pid := cmd.Process.Pid
			if pid <= 0 {
				cancelErr = os.ErrProcessDone
				return
			}

			pgid, err := syscall.Getpgid(pid)
			if err != nil {
				if errors.Is(err, syscall.ESRCH) {
					cancelErr = os.ErrProcessDone
					return
				}
				pgid = pid
			}

			if err := syscall.Kill(-pgid, syscall.SIGTERM); err != nil && !errors.Is(err, syscall.ESRCH) {
				cancelErr = err
				return
			}

			go func(groupID int) {
				timer := time.NewTimer(CancelGracePeriod)
				defer timer.Stop()
				<-timer.C
				_ = syscall.Kill(-groupID, syscall.SIGKILL)
			}(pgid)
		})
		return cancelErr
	}
}
