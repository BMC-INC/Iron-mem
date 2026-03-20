# IronMem Demo GIF
# Run: vhs assets/demo.tape

Output assets/demo.gif

Set Shell "zsh"
Set FontSize 15
Set Width 820
Set Height 480
Set Theme "Catppuccin Mocha"
Set Padding 16
Set TypingSpeed 50ms

# Check status
Type "ironmem status"
Enter
Sleep 2.5s

# List memories
Type "ironmem list"
Enter
Sleep 3.5s

# Search
Type 'ironmem search "auth"'
Enter
Sleep 3s

# Inject context
Type "ironmem inject"
Enter
Sleep 1.5s

# Show the file
Type "head -20 IRONMEM.md"
Enter
Sleep 4s
