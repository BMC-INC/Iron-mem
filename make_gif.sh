#!/bin/bash
#
# Generates an animated GIF demo for IronMem.
# Requires ImageMagick to be installed.

# DEBUGGING: Print every command and continue on error
set -x
# set -e

# --- Configuration ---
# Directory to store intermediate frames and the final GIF
DEMO_DIR="ironmem_demo"
# Output GIF name
GIF_NAME="ironmem_demo.gif"
# Frame prefix
FRAME_PREFIX="frame"

# --- Style Configuration (adjust as needed) ---
# Dimensions
WIDTH=800
HEIGHT=450
# Colors
BG_COLOR="#1E1E1E" # Dark background
TEXT_COLOR="#D4D4D4" # Light text
# Font
FONT_SIZE=14
# Animation delays (in 1/100s of a second)
NORMAL_DELAY=150 # 1.5 seconds
LONG_DELAY=300   # 3.0 seconds

# --- Helper Functions ---
function render_frame() {
  echo "--- Rendering frame: $2 ---"
  local text_content="$1"
  local output_file="$2"
  
  # Use 'caption' for robust text wrapping without an explicit font.
  magick -size "${WIDTH}x${HEIGHT}" \
         -background "$BG_COLOR" \
         -fill "$TEXT_COLOR" \
         -pointsize "$FONT_SIZE" \
         -gravity NorthWest \
         caption:"$text_content" \
         "$output_file"
  
  echo "✅ Rendered $output_file"
}

# --- Main Script ---

# 1. Setup
echo "🎬 Starting GIF generation for IronMem..."
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"

# --- Frame Generation ---

# Frame 01: The Problem
read -r -d '' FRAME_01_TEXT << EOM
$ ai what did we work on last session?

🤖 I'm sorry, I don't have any memory of previous sessions.
   My knowledge is limited to the current conversation.
   How can I help you today?
EOM
render_frame "$FRAME_01_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_01.png"

# Frame 02: A new hope
read -r -d '' FRAME_02_TEXT << EOM
Let's fix that. Installing IronMem...

$ curl -fsSL https://.../install.sh | bash
...
✅ IronMem installed successfully!
EOM
render_frame "$FRAME_02_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_02.png"

# Frame 03: Session 1 - The Task
read -r -d '' FRAME_03_TEXT << EOM
# --- First Session ---

$ ai create a simple web server in python
EOM
render_frame "$FRAME_03_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_03.png"

# Frame 04: Session 1 - AI Response
read -r -d '' FRAME_04_TEXT << EOM
🤖 Certainly! Here is a minimal web server using Flask:

# main.py
from flask import Flask

app = Flask(__name__)

@app.route('/')
def hello_world():
    return 'Hello, World!'

if __name__ == '__main__':
    app.run(debug=True)
EOM
render_frame "$FRAME_04_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_04.png"

# Frame 05: Session End
read -r -d '' FRAME_05_TEXT << EOM
# --- Session Ends ---

$ # ...user logs off...

(IronMem background process)
✅ Session activity captured.
🧠 Compressing memories with Claude API...
✨ New memory stored.
EOM
render_frame "$FRAME_05_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_05.png"

# Frame 06: New Session
read -r -d '' FRAME_06_TEXT << EOM
# --- A New Session Starts ---

(IronMem hook: session-start.sh)
✨ Found 1 recent memory.
✅ Injected into IRONMEM.md
EOM
render_frame "$FRAME_06_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_06.png"

# Frame 07: The Magic File
read -r -d '' FRAME_07_TEXT << EOM
$ cat IRONMEM.md

# Recent Memories (last 5 sessions)

- [Session yesterday] Created a basic Flask web server in \`main.py\` to serve a 'Hello, World!' message at the root endpoint.
EOM
render_frame "$FRAME_07_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_07.png"

# Frame 08: The Payoff
read -r -d '' FRAME_08_TEXT << EOM
$ ai what did we work on last session?
EOM
render_frame "$FRAME_08_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_08.png"

# Frame 09: The Smart AI
read -r -d '' FRAME_09_TEXT << EOM
🤖 According to my notes in IRONMEM.md, last session we created a basic "Hello, World!" web server using Flask in the file \`main.py\`.

   How would you like to build on that today?
EOM
render_frame "$FRAME_09_TEXT" "$DEMO_DIR/${FRAME_PREFIX}_09.png"


# Frame 10: Final Branding
echo "--- Rendering frame: $DEMO_DIR/${FRAME_PREFIX}_10.png ---"
# For this frame, we'll center the logo and text
magick "$DEMO_DIR/${FRAME_PREFIX}_09.png" \
       logo.png -gravity center -geometry +0-50 -composite \
       -fill "$TEXT_COLOR" \
       -gravity center -pointsize 24 -annotate +0+50 "IronMem" \
       -gravity center -pointsize 16 -annotate +0+80 "Your AI's memory, forged in Rust." \
       "$DEMO_DIR/${FRAME_PREFIX}_10.png"
echo "✅ Rendered $DEMO_DIR/${FRAME_PREFIX}_10.png"


# 2. Assemble GIF
echo "✨ Assembling the GIF..."
magick -delay $NORMAL_DELAY "$DEMO_DIR/${FRAME_PREFIX}_"*.png \
       -set delay $LONG_DELAY "$DEMO_DIR/${FRAME_PREFIX}_09.png" \
       -set delay $LONG_DELAY "$DEMO_DIR/${FRAME_PREFIX}_10.png" \
       -loop 0 "$DEMO_DIR/$GIF_NAME"

# 3. Cleanup
# rm -f "$DEMO_DIR/${FRAME_PREFIX}_"*.png
echo "🎉 Done! Your demo is ready at: $DEMO_DIR/$GIF_NAME"
echo "ℹ️  Intermediate frames were kept in $DEMO_DIR for inspection."

exit 0
