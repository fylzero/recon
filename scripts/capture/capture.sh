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
WINDOW_X=100
WINDOW_Y=100

# Reference window dimensions (2x retina downscaled to 1024px wide)
REF_WIDTH=1024
REF_HEIGHT=640

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
while [[ $# -gt 0 ]]; do
  case $1 in
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --only)
      ONLY_SHOT="$2"
      shift 2
      ;;
    --only=*)
      ONLY_SHOT="${1#*=}"
      shift
      ;;
    *)
      shift
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

success "Prerequisites OK"

# Check if non-demo Recon is already running
if pgrep -f "Recon.app" >/dev/null 2>&1; then
  # Check if it's the demo instance by checking HOME env
  RUNNING_PIDS=$(pgrep -f "Recon.app")
  for pid in $RUNNING_PIDS; do
    # If we can't determine it's a demo instance, warn and exit
    warn "Recon is already running (PID $pid)."
    warn "Please quit Recon before running this script to avoid killing the wrong instance."
    die "Aborting to protect existing Recon instance."
  done
fi

# Generate demo database
log "Generating demo database..."
bun run "$SCRIPT_DIR/generate-demo-db.ts"
success "Demo database created"

# Set up isolated app data
log "Setting up isolated app data..."
bun run "$SCRIPT_DIR/setup-demo-connection.ts"
success "Isolated app data configured"

# Launch Recon with isolated HOME
log "Launching Recon with isolated environment..."
HOME="$ISOLATED_HOME" "$APP_BINARY" >/dev/null 2>&1 &
APP_PID=$!
sleep 4

# Wait for window to appear
log "Waiting for Recon window..."
for i in {1..30}; do
  if osascript -e 'tell application "System Events" to exists (first window of process "Recon")' 2>/dev/null; then
    break
  fi
  sleep 0.5
done

# Get window ID helper using inline JXA
get_window_id() {
  osascript -l JavaScript -e 'ObjC.import("CoreGraphics"); var l=ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly,0))); l.filter(w=>w.kCGWindowOwnerName=="Recon"&&w.kCGWindowLayer==0).map(w=>w.kCGWindowNumber).join("")'
}

WINDOW_ID=$(get_window_id)
if [[ -z "$WINDOW_ID" ]]; then
  die "Could not find Recon window ID"
fi
success "Window ID: $WINDOW_ID"

# Set window size and position
log "Setting window size to ${WINDOW_WIDTH}x${WINDOW_HEIGHT}..."
osascript <<EOF
tell application "System Events"
  tell process "Recon"
    set position of window 1 to {$WINDOW_X, $WINDOW_Y}
    set size of window 1 to {$WINDOW_WIDTH, $WINDOW_HEIGHT}
  end tell
end tell
EOF
sleep 1

# Re-get window ID after resize (it may change)
WINDOW_ID=$(get_window_id)
success "Window positioned (ID: $WINDOW_ID)"

# Helper to convert reference coordinates to screen coordinates
# Reference: 1024x640 logical window
# Actual: 1440x900 at position (100, 100)
click_ref() {
  local ref_x=$1
  local ref_y=$2
  local screen_x=$(( WINDOW_X + ref_x * WINDOW_WIDTH / REF_WIDTH ))
  local screen_y=$(( WINDOW_Y + ref_y * WINDOW_HEIGHT / REF_HEIGHT ))
  
  # Bring Recon to front
  osascript -e 'tell application "System Events" to set frontmost of process "Recon" to true' 2>/dev/null
  
  # Click at coordinates (ignore stderr which contains harmless -25208 and element name)
  osascript -e "tell application \"System Events\" to click at {$screen_x, $screen_y}" 2>/dev/null || true
  sleep 0.3
}

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
    # Ensure screenshots directory exists
    mkdir -p "$SCREENSHOTS_DIR"
    
    # Capture with screencapture
    if screencapture -l "$WINDOW_ID" -o "$output_path" 2>/dev/null && [[ -f "$output_path" ]]; then
      success "Saved: $output_path"
    else
      error "Screenshot capture failed - check Screen Recording permission"
      error "Go to System Settings → Privacy & Security → Screen Recording"
      error "and enable your terminal app (Terminal, iTerm, etc.)"
      return 1
    fi
  fi
}

# Screenshot 1: Connection dashboard
log ""
log "=== Screenshot 1: Connection Dashboard ==="
sleep 1.5
capture_screenshot "01-connections" "Connection dashboard"

# Open the demo connection
log "Opening Demo Store connection..."
click_ref 120 181
sleep 2.5

# Screenshot 2: Table browser with data grid (customers table)
log ""
log "=== Screenshot 2: Table Browser ==="

# Make sure Tables tab is active
click_ref 62 85
sleep 0.5

# Click on customers table in the sidebar
click_ref 45 169
sleep 1.5

capture_screenshot "02-table-browser" "Table browser with data grid"

# Screenshot 3: Query editor with results
log ""
log "=== Screenshot 3: Query Editor ==="

# Switch to SQL tab
click_ref 120 85
sleep 1

# Click in the editor
click_ref 400 200
sleep 0.3

# Type query on one line (will be beautified)
osascript <<'EOF'
tell application "System Events"
  keystroke "SELECT o.order_number, c.full_name AS customer, o.status, o.total_cents / 100.0 AS total, COUNT(oi.id) AS items FROM orders o JOIN customers c ON c.id = o.customer_id JOIN order_items oi ON oi.order_id = o.id GROUP BY o.id ORDER BY o.created_at DESC LIMIT 20;"
end tell
EOF
sleep 0.5

# Click Beautify button
click_ref 926 133
sleep 0.8

# Click Run button
click_ref 27 133
sleep 2

capture_screenshot "03-query-editor" "Query editor with results"

# Screenshot 4: Schema diagram with polymorphic edges
log ""
log "=== Screenshot 4: Schema Diagram ==="

# Switch to Diagram tab
click_ref 175 85
sleep 2

# Click "2 polymorphic" pill to enable polymorphic edges
click_ref 396 111
sleep 0.5

# Click on empty canvas to remove focus ring from pill
click_ref 100 400
sleep 0.3

# Click Fit to center the diagram
click_ref 952 111
sleep 1

capture_screenshot "04-schema-diagram" "Schema diagram with polymorphic edges"

# Screenshot 5: Table structure (orders table)
log ""
log "=== Screenshot 5: Table Structure ==="

# Switch back to Tables tab
click_ref 62 85
sleep 0.8

# Click on orders table
click_ref 45 203
sleep 1.5

# Click Structure sub-tab
click_ref 273 134
sleep 1

capture_screenshot "05-table-structure" "Table structure with columns and foreign keys"

# Record demo GIF
if [[ -z "$ONLY_SHOT" ]]; then
  log ""
  log "=== Recording Demo GIF ==="
  
  TEMP_VIDEO="/tmp/recon-demo-$$.mov"
  GIF_FAILED=false
  
  if [[ "$DRY_RUN" == true ]]; then
    warn "DRY RUN: Would record demo and create GIF at $DEMO_GIF_PATH"
  else
    log "Recording demo flow (15 seconds)..."
    
    # Try to record with screencapture
    if command -v screencapture >/dev/null 2>&1; then
      # Start recording in background
      screencapture -v "$TEMP_VIDEO" 2>/dev/null &
      RECORD_PID=$!
      
      # Perform demo flow
      sleep 1
      
      # Go back to connections
      osascript -e 'tell application "System Events" to keystroke "w" using {command down}' 2>/dev/null || true
      sleep 1.5
      
      # Open connection again
      click_ref 120 181
      sleep 2
      
      # Show customers table
      click_ref 62 85
      sleep 0.5
      click_ref 45 169
      sleep 2
      
      # Switch to products
      click_ref 45 220
      sleep 2
      
      # Open Diagram
      click_ref 175 85
      sleep 2
      
      # Toggle polymorphic
      click_ref 396 111
      sleep 1
      
      # Switch to SQL
      click_ref 120 85
      sleep 2
      
      # Stop recording
      kill -INT "$RECORD_PID" 2>/dev/null || true
      wait "$RECORD_PID" 2>/dev/null || true
      sleep 1
      
      if [[ -f "$TEMP_VIDEO" ]]; then
        log "Converting to GIF..."
        
        # Generate palette for better quality
        if ffmpeg -i "$TEMP_VIDEO" -vf "fps=12,scale=960:-1:flags=lanczos,palettegen=stats_mode=diff" -y /tmp/recon-palette.png 2>/dev/null; then
          # Create GIF with palette
          if ffmpeg -i "$TEMP_VIDEO" -i /tmp/recon-palette.png -lavfi "fps=12,scale=960:-1:flags=lanczos [x]; [x][1:v] paletteuse=dither=bayer:bayer_scale=5" -y "$DEMO_GIF_PATH" 2>/dev/null; then
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
            error "GIF conversion failed"
            GIF_FAILED=true
          fi
        else
          error "Palette generation failed"
          GIF_FAILED=true
        fi
      else
        error "Screen recording failed - no video file created"
        error "This is optional; screenshots were captured successfully"
        GIF_FAILED=true
      fi
    else
      error "screencapture not available for video recording"
      GIF_FAILED=true
    fi
    
    if [[ "$GIF_FAILED" == true ]]; then
      warn "Demo GIF recording failed, but screenshots are complete"
      warn "You can create the GIF manually or skip it"
    fi
  fi
fi

# Quit Recon
log ""
log "Cleaning up..."

# Quit gracefully first
osascript -e 'quit app "Recon"' 2>/dev/null || true
sleep 1

# Force kill our demo instance if still running
if kill -0 "$APP_PID" 2>/dev/null; then
  kill "$APP_PID" 2>/dev/null || true
  sleep 0.5
fi

success ""
success "Screenshot capture complete!"
log ""
log "Screenshots saved to: $SCREENSHOTS_DIR/"
if [[ -z "$ONLY_SHOT" && "$GIF_FAILED" != true ]]; then
  log "Demo GIF saved to: $DEMO_GIF_PATH"
fi
log ""
log "Review the screenshots and commit them:"
log "  git add docs/screenshots/*.png docs/demo.gif"
log "  git commit -m \"Update screenshots and demo\""
log "  git push"
