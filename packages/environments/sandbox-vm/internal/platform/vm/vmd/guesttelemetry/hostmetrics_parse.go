package guesttelemetry

import (
	"bufio"
	"fmt"
	"strconv"
	"strings"
)

func parseCPUStat(contents string) (total uint64, idle uint64, err error) {
	scanner := bufio.NewScanner(strings.NewReader(contents))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if !strings.HasPrefix(line, "cpu ") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 5 {
			return 0, 0, fmt.Errorf("invalid cpu line")
		}

		var sum uint64
		for i := 1; i < len(fields); i++ {
			v, parseErr := strconv.ParseUint(fields[i], 10, 64)
			if parseErr != nil {
				return 0, 0, fmt.Errorf("parse cpu field: %w", parseErr)
			}
			sum += v
			if i == 4 {
				idle = v
			}
		}
		return sum, idle, nil
	}
	if err := scanner.Err(); err != nil {
		return 0, 0, err
	}
	return 0, 0, fmt.Errorf("cpu line not found")
}

func parseMemInfo(contents string) (totalBytes uint64, availableBytes uint64, err error) {
	scanner := bufio.NewScanner(strings.NewReader(contents))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if strings.HasPrefix(line, "MemTotal:") {
			totalBytes, err = parseMemInfoLineBytes(line)
			if err != nil {
				return 0, 0, err
			}
		}
		if strings.HasPrefix(line, "MemAvailable:") {
			availableBytes, err = parseMemInfoLineBytes(line)
			if err != nil {
				return 0, 0, err
			}
		}
	}
	if err := scanner.Err(); err != nil {
		return 0, 0, err
	}
	if totalBytes == 0 || availableBytes == 0 {
		return 0, 0, fmt.Errorf("missing meminfo fields")
	}
	return totalBytes, availableBytes, nil
}

func parseMemInfoLineBytes(line string) (uint64, error) {
	fields := strings.Fields(line)
	if len(fields) < 2 {
		return 0, fmt.Errorf("invalid meminfo line")
	}
	valueKB, err := strconv.ParseUint(fields[1], 10, 64)
	if err != nil {
		return 0, fmt.Errorf("parse meminfo value: %w", err)
	}
	return valueKB * 1024, nil
}

func parseNetDev(contents string) (rxBytes uint64, txBytes uint64, err error) {
	scanner := bufio.NewScanner(strings.NewReader(contents))
	for scanner.Scan() {
		line := scanner.Text()
		if !strings.Contains(line, ":") {
			continue
		}

		parts := strings.SplitN(line, ":", 2)
		if len(parts) != 2 {
			continue
		}

		iface := strings.TrimSpace(parts[0])
		if iface == "" || iface == "lo" {
			continue
		}

		fields := strings.Fields(parts[1])
		if len(fields) < 16 {
			continue
		}

		rx, err := strconv.ParseUint(fields[0], 10, 64)
		if err != nil {
			return 0, 0, fmt.Errorf("parse rx bytes: %w", err)
		}
		tx, err := strconv.ParseUint(fields[8], 10, 64)
		if err != nil {
			return 0, 0, fmt.Errorf("parse tx bytes: %w", err)
		}

		rxBytes += rx
		txBytes += tx
	}
	if err := scanner.Err(); err != nil {
		return 0, 0, err
	}
	return rxBytes, txBytes, nil
}

func parseDiskStats(contents string) (readBytes uint64, writeBytes uint64, err error) {
	scanner := bufio.NewScanner(strings.NewReader(contents))
	for scanner.Scan() {
		fields := strings.Fields(scanner.Text())
		if len(fields) < 14 {
			continue
		}

		name := fields[2]
		if name == "" || strings.HasPrefix(name, "loop") || isPartitionDevice(name) {
			continue
		}

		sectorsRead, err := strconv.ParseUint(fields[5], 10, 64)
		if err != nil {
			return 0, 0, fmt.Errorf("parse sectors read: %w", err)
		}
		sectorsWritten, err := strconv.ParseUint(fields[9], 10, 64)
		if err != nil {
			return 0, 0, fmt.Errorf("parse sectors written: %w", err)
		}

		const sectorBytes = 512
		readBytes += sectorsRead * sectorBytes
		writeBytes += sectorsWritten * sectorBytes
	}
	if err := scanner.Err(); err != nil {
		return 0, 0, err
	}
	return readBytes, writeBytes, nil
}

func isPartitionDevice(name string) bool {
	name = strings.TrimSpace(name)
	if name == "" {
		return false
	}
	last := name[len(name)-1]
	return last >= '0' && last <= '9'
}
