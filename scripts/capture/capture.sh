#!/bin/bash
#
# Automated screenshot capture for Recon README.
# Captures the five required screenshots and a demo GIF.
#
# Requirements:
# - macOS with Screen Recording permission for Terminal
# - Recon installed at /Applications/Recon.app
# - bun (for demo data generation)
# - ffmpeg (for GIF conversion)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCREENSHOTS_DIR="$PROJECT_DIR/docs/screenshots"
DEMO_GIF_PATH="$PROJECT_DIR/docs/demo.gif"

# Configuration
TEMP_DIR="${RECON_DEMO_DB_DIR:-/tmp/recon-demo}"
ISOLATED_HOME="$TEMP_DIR/home"
APP_PATH="/Applications/Recon.app"
APP_BINARY="$APP_PATH/Contents/MacOS/Recon"
WINDOW_WIDTH=1440
WINDOW_HEIGHT=900

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Options
DRY_RUN=false
ONLY_SHOT=""

# Parse arguments
for arg in "$@"; do
  case $arg in
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --only)
      ONLY_SHOT="$2"
      shift 2
      ;;
    --only=*)
      ONLY_SHOT="${arg#*=}"
      shift
      ;;
    *)
      ;;
  esac
done

# Print helpers
log() { echo -e "${BLUE}▸${NC} $*"; }
success() { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
error() { echo -e "${RED}✗${NC} $*"; }
die() { error "$*"; exit 1; }

# Check prerequisites
log "Checking prerequisites..."

[[ -d "$APP_PATH" ]] || die "Recon not found at $APP_PATH"
command -v bun >/dev/null 2>&1 || die "bun not found. Install from https://bun.sh"
command -v ffmpeg >/dev/null 2>&1 || die "ffmpeg not found. Install with: brew install ffmpeg"
command -v osascript >/dev/null 2>&1 || die "osascript not found (macOS only)"

# Check if we have screen recording permission (best effort)
if ! screencapture -l 1 -o /tmp/test-capture-$$.png 2>/dev/null; then
  warn "Screen recording permission may not be granted to Terminal."
  warn "Go to System Settings → Privacy & Security → Screen Recording"
  rm -f /tmp/test-capture-$$.png 2>/dev/null || true
else
  rm -f /tmp/test-capture-$$.png 2>/dev/null || true
fi

success "Prerequisites OK"

# Generate demo database
log "Generating demo database..."
bun run "$SCRIPT_DIR/generate-demo-db.ts"
success "Demo database created"

# Set up isolated app data
log "Setting up isolated app data..."
bun run "$SCRIPT_DIR/setup-demo-connection.ts"
success "Isolated app data configured"

# Kill any existing Recon instances
if pgrep -f "Recon.app" >/dev/null 2>&1; then
  log "Stopping existing Recon instances..."
  pkill -f "Recon.app" || true
  sleep 1
fi

# Launch Recon with isolated HOME
log "Launching Recon with isolated environment..."
HOME="$ISOLATED_HOME" "$APP_BINARY" >/dev/null 2>&1 &
APP_PID=$!
sleep 3

# Wait for window to appear
log "Waiting for Recon window..."
for i in {1..30}; do
  if osascript -e 'tell application "System Events" to exists (first window of process "Recon")' 2>/dev/null; then
    break
  fi
  sleep 0.5
done

# Get window ID for screencapture
log "Getting window ID..."
WINDOW_ID=$(osascript "$SCRIPT_DIR/get-window-id.jxa")
success "Window ID: $WINDOW_ID"

# Set window size and position
log "Setting window size to ${WINDOW_WIDTH}x${WINDOW_HEIGHT}..."
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    set position of window 1 to {100, 100}
    set size of window 1 to {$WINDOW_WIDTH, $WINDOW_HEIGHT}
  end tell
end tell
EOF
sleep 1
success "Window positioned"

# Helper function to capture a screenshot
capture_screenshot() {
  local name="$1"
  local description="$2"
  local output_path="$SCREENSHOTS_DIR/$name.png"
  
  if [[ -n "$ONLY_SHOT" && "$ONLY_SHOT" != "$name" ]]; then
    return
  fi
  
  log "Capturing: $description"
  
  if [[ "$DRY_RUN" == true ]]; then
    warn "DRY RUN: Would capture to $output_path"
  else
    screencapture -l "$WINDOW_ID" -o "$output_path"
    success "Saved: $output_path"
  fi
}

# Helper to send keystrokes
send_keys() {
  osascript -e "tell application \"System Events\" to keystroke \"$1\""
  sleep 0.3
}

send_keys_with_modifiers() {
  local key="$1"
  local modifiers="$2"
  osascript -e "tell application \"System Events\" to keystroke \"$key\" using {$modifiers}"
  sleep 0.3
}

# Helper to click menu item
click_menu() {
  local menu_name="$1"
  local item_name="$2"
  osascript <<EOF
tell application "System Events"
  tell process "Recon"
    click menu item "$item_name" of menu "$menu_name" of menu bar 1
  end tell
end tell
EOF
  sleep 0.5
}

# Screenshot 1: Main connection dashboard showing grouped connections
log ""
log "=== Screenshot 1: Connection Dashboard ==="
sleep 1
capture_screenshot "01-connections" "Connection dashboard"

# Open the demo connection by clicking on it
log "Opening demo connection..."
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    -- Click on "Demo Store" connection
    click button "Demo Store" of window 1
  end tell
end tell
EOF
sleep 2

# Screenshot 2: Table browser with data grid
log ""
log "=== Screenshot 2: Table Browser with Data Grid ==="

# Open customers table
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    -- Open sidebar if needed (Cmd+B)
    keystroke "b" using {command down}
  end tell
end tell
EOF
sleep 0.5

# Click on customers table
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click static text "customers" of window 1
    end try
  end tell
end tell
EOF
sleep 1.5

capture_screenshot "02-table-browser" "Table browser with data grid"

# Screenshot 3: Query editor with syntax highlighting and results
log ""
log "=== Screenshot 3: Query Editor ==="

# Open query editor (look for a button or use keyboard shortcut)
# Try to click "Query" or similar - this might need adjustment based on actual UI
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      -- Look for Query tab or button
      click button "Query" of window 1
    on error
      -- Fallback: try to find query editor some other way
      keystroke "n" using {command down}
    end try
  end tell
end tell
EOF
sleep 1

# Type a sample query
send_keys_with_modifiers "a" "command down"
sleep 0.2

osascript <<EOF
tell application "System Events"
  tell process "Recon"
    keystroke "SELECT 
  o.order_number,
  c.full_name AS customer,
  o.status,
  o.total_cents / 100.0 AS total,
  COUNT(oi.id) AS items
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_items oi ON oi.order_id = o.id
GROUP BY o.id
ORDER BY o.created_at DESC;"
  end tell
end tell
EOF
sleep 0.5

# Execute query (Cmd+Enter or similar)
send_keys_with_modifiers "return" "command down"
sleep 1.5

capture_screenshot "03-query-editor" "Query editor with results"

# Screenshot 4: Schema diagram showing table relationships
log ""
log "=== Screenshot 4: Schema Diagram ==="

# Open schema diagram - look for a button or menu item
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      -- Try clicking Schema or Diagram tab/button
      click button "Diagram" of window 1
    on error
      -- Alternative approach
      keystroke "d" using {command down, shift down}
    end try
  end tell
end tell
EOF
sleep 2

capture_screenshot "04-schema-diagram" "Schema diagram showing relationships"

# Screenshot 5: Table structure view with columns, indexes, and foreign keys
log ""
log "=== Screenshot 5: Table Structure ==="

# Go back to a table and open structure view
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      -- Click on orders table
      click static text "orders" of window 1
    end try
  end tell
end tell
EOF
sleep 1

# Click Structure tab
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click button "Structure" of window 1
    end try
  end tell
end tell
EOF
sleep 1

capture_screenshot "05-table-structure" "Table structure with columns and foreign keys"

# Record demo GIF
if [[ -z "$ONLY_SHOT" ]]; then
  log ""
  log "=== Recording Demo GIF ==="
  
  TEMP_VIDEO="/tmp/recon-demo-$$.mov"
  
  if [[ "$DRY_RUN" == true ]]; then
    warn "DRY RUN: Would record demo and create GIF at $DEMO_GIF_PATH"
  else
    # Navigate to show a representative flow
    log "Recording 12-second demo..."
    
    # Start recording
    screencapture -v -V "$TEMP_VIDEO" &
    RECORD_PID=$!
    
    # Perform demo actions
    sleep 1
    
    # Show customers table
    osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click static text "customers" of window 1
    end try
  end tell
end tell
EOF
    sleep 2
    
    # Switch to products
    osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click static text "products" of window 1
    end try
  end tell
end tell
EOF
    sleep 2
    
    # Show orders
    osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click static text "orders" of window 1
    end try
  end tell
end tell
EOF
    sleep 2
    
    # Open diagram
    osascript <<EOF
tell application "System Events"
  tell process "Recon"
    try
      click button "Diagram" of window 1
    end try
  end tell
end tell
EOF
    sleep 3
    
    # Stop recording
    kill -INT "$RECORD_PID" 2>/dev/null || true
    wait "$RECORD_PID" 2>/dev/null || true
    
    sleep 1
    
    if [[ -f "$TEMP_VIDEO" ]]; then
      log "Converting to GIF..."
      
      # Generate palette for better quality
      ffmpeg -i "$TEMP_VIDEO" -vf "fps=15,scale=960:-1:flags=lanczos,palettegen=stats_mode=diff" -y /tmp/recon-palette.png 2>/dev/null
      
      # Create GIF with palette
      ffmpeg -i "$TEMP_VIDEO" -i /tmp/recon-palette.png -lavfi "fps=15,scale=960:-1:flags=lanczos [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=5" -y "$DEMO_GIF_PATH" 2>/dev/null
      
      # Clean up
      rm -f "$TEMP_VIDEO" /tmp/recon-palette.png
      
      # Check file size
      GIF_SIZE=$(du -h "$DEMO_GIF_PATH" | cut -f1)
      success "Demo GIF created: $DEMO_GIF_PATH ($GIF_SIZE)"
      
      # Warn if over 8MB
      GIF_SIZE_BYTES=$(stat -f%z "$DEMO_GIF_PATH" 2>/dev/null || stat -c%s "$DEMO_GIF_PATH")
      if [[ $GIF_SIZE_BYTES -gt 8388608 ]]; then
        warn "GIF size is over 8MB. Consider reducing length or quality."
      fi
    else
      error "Recording failed - no video file created"
    fi
  fi
fi

# Quit Recon
log ""
log "Cleaning up..."
osascript -e 'quit app "Recon"' 2>/dev/null || true
sleep 1

# Force kill if still running
if kill -0 "$APP_PID" 2>/dev/null; then
  kill "$APP_PID" 2>/dev/null || true
fi

success ""
success "Screenshot capture complete!"
log ""
log "Screenshots saved to: $SCREENSHOTS_DIR/"
[[ -z "$ONLY_SHOT" ]] && log "Demo GIF saved to: $DEMO_GIF_PATH"
log ""
log "Note: The actual UI navigation may need adjustments based on the app's exact"
log "      button labels and structure. Test this script and refine as needed."
