#import <Cocoa/Cocoa.h>
#include <fcntl.h>
#include <unistd.h>
int main(int argc, const char **argv) {
  @autoreleasepool {
    if (argc != 2) return 2;
    NSApplication *app = [NSApplication sharedApplication];
    [app setActivationPolicy:NSApplicationActivationPolicyRegular];
    NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(200,200,480,240)
      styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    [window setTitle:@"Synthetic AX fixture"];
    NSTextView *editor = [[NSTextView alloc] initWithFrame:NSMakeRect(0,0,480,240)];
    [editor setString:@"Synthetic fixture only."];
    [editor setSelectedRange:NSMakeRange(0,0)];
    [window setContentView:editor];
    [window makeKeyAndOrderFront:nil];
    [window makeFirstResponder:editor];
    [app activateIgnoringOtherApps:YES];
    fcntl(STDIN_FILENO, F_SETFL, O_NONBLOCK);
    [NSTimer scheduledTimerWithTimeInterval:0.02 repeats:YES block:^(NSTimer *timer) {
      char c; ssize_t n = read(STDIN_FILENO, &c, 1);
      if (n == 0 || (n == 1 && c == 'Q')) [app terminate:nil];
    }];
    dispatch_async(dispatch_get_main_queue(), ^{
      printf("READY %s %s\n", argv[1], [[[NSBundle mainBundle] bundleIdentifier] UTF8String]);
      fflush(stdout);
    });
    [app run];
  }
  return 0;
}
