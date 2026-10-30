// W1 loader test: an entry point that returns. The process must end with its
// return value (7), as on Windows, instead of returning to address 0.
int start(void) { return 7; }
