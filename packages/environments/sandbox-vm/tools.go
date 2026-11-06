//go:build tools

package sandbox

import (
	_ "golang.org/x/mobile/bind"
	_ "golang.org/x/mobile/bind/objc"
	_ "golang.org/x/mobile/cmd/gobind"
	_ "golang.org/x/mobile/cmd/gomobile"
)
