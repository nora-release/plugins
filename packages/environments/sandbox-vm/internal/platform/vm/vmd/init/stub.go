//go:build !linux

package init

import "fmt"

func Run() error {
	return fmt.Errorf("init is only supported on Linux")
}
