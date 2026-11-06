package guesttelemetry

import (
	"fmt"
	"strconv"
	"strings"
)

type processStat struct {
	PID   int
	Comm  string
	UTime uint64
	STime uint64
}

func parseProcStatLine(line string) (processStat, error) {
	line = strings.TrimSpace(line)
	if line == "" {
		return processStat{}, fmt.Errorf("empty stat line")
	}

	lparen := strings.IndexByte(line, '(')
	rparen := strings.LastIndexByte(line, ')')
	if lparen < 0 || rparen < 0 || rparen <= lparen {
		return processStat{}, fmt.Errorf("invalid stat format")
	}

	pidStr := strings.TrimSpace(line[:lparen])
	pid, err := strconv.Atoi(pidStr)
	if err != nil {
		return processStat{}, fmt.Errorf("parse pid: %w", err)
	}

	comm := line[lparen+1 : rparen]
	rest := strings.Fields(line[rparen+1:])
	if len(rest) < 13 {
		return processStat{}, fmt.Errorf("invalid stat fields")
	}

	utime, err := strconv.ParseUint(rest[11], 10, 64)
	if err != nil {
		return processStat{}, fmt.Errorf("parse utime: %w", err)
	}
	stime, err := strconv.ParseUint(rest[12], 10, 64)
	if err != nil {
		return processStat{}, fmt.Errorf("parse stime: %w", err)
	}

	return processStat{
		PID:   pid,
		Comm:  comm,
		UTime: utime,
		STime: stime,
	}, nil
}
