#!/bin/bash
# Configure DHCP lease time to prevent IP exhaustion
# Based on Tart's recommendation: https://tart.run/faq/

set -e

LEASE_TIME=600  # 10 minutes (600 seconds)
PLIST_PATH="/Library/Preferences/SystemConfiguration/com.apple.InternetSharing.default.plist"

echo "==> Configuring DHCP lease time to ${LEASE_TIME} seconds..."

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run with sudo privileges"
    echo "Usage: sudo $0"
    exit 1
fi

# Backup existing plist if it exists
if [ -f "$PLIST_PATH" ]; then
    BACKUP_PATH="${PLIST_PATH}.backup.$(date +%Y%m%d_%H%M%S)"
    echo "==> Backing up existing plist to: $BACKUP_PATH"
    cp "$PLIST_PATH" "$BACKUP_PATH"
fi

# Configure DHCP lease time
echo "==> Setting DHCPLeaseTimeSecs to ${LEASE_TIME}"
defaults write "$PLIST_PATH" bootpd -dict DHCPLeaseTimeSecs -int $LEASE_TIME

# Verify the change
echo "==> Verifying configuration..."
if defaults read "$PLIST_PATH" bootpd 2>/dev/null | grep -q "DHCPLeaseTimeSecs = $LEASE_TIME"; then
    echo "✓ DHCP lease time successfully configured"
    echo ""
    echo "IMPORTANT: You may need to restart networking or reboot for changes to take effect"
    echo "To restart networking:"
    echo "  1. Turn off Internet Sharing in System Preferences"
    echo "  2. Turn it back on"
    echo "Or simply reboot your Mac"
else
    echo "✗ Failed to verify DHCP lease time configuration"
    exit 1
fi
