/*
 * Phase 2.3 accept-key strategy spike (ROADMAP Tier 1.1 / cross-platform plan).
 *
 * THE QUESTION. compme's accept tap must swallow Tab/Esc *only* while a ghost
 * suggestion is visible, and let them through otherwise. macOS does this with a
 * CGEventTap that returns NULL to consume. X11 has no equivalent "callback that
 * may consume", and the plan listed three candidates:
 *   1. XInput2 raw key events   - passive: you observe the key, you cannot stop
 *                                 it, so accepting with Tab would also insert a
 *                                 literal Tab. Fails the requirement outright.
 *   2. XGrabKey                 - active: the plan called it "too invasive"
 *                                 because grabs are exclusive.
 *   3. KeyInterceptMode::FocusScopedInhibit - degrade, no interception.
 *
 * What (2) actually offers, and what this spike measures, is the pairing the
 * plan did not consider: a passive grab established with keyboard
 * GrabModeSync freezes key processing and hands the client the event, which it
 * then resolves with XAllowEvents - AsyncKeyboard to CONSUME, or ReplayKeyboard
 * to deliver the key to the focused application as if no grab existed. If that
 * works, a grab held only while a suggestion is visible gives exactly the macOS
 * semantics, with no synthetic re-send and no window in which a keystroke is
 * lost or duplicated.
 *
 * THE MEASUREMENT. Against the GTK fixture (which logs every key it receives):
 *   A. baseline    - no grab, synthesize Tab, the app must receive it
 *   B. consume     - grab held, synthesize Tab, AsyncKeyboard: the app must NOT
 *   C. pass-through- grab held, synthesize Tab, ReplayKeyboard: the app MUST
 *   D. released    - grab dropped, synthesize Tab, the app must receive it
 * B and C together are the requirement; A and D prove the rig itself works, so
 * a "no key ever arrives" bug cannot masquerade as a successful consume.
 *
 * Run through the session harness:
 *   run-linux-atspi-session.sh --run-in-session <this binary>
 *
 * Exit codes: 0 all four observations as required · 1 a required observation
 * failed (verdict printed) · 2 rig unusable (no display/XTEST/fixture window).
 */
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* The fixture's window title, set by linux-atspi-fixture.c. */
#define FIXTURE_TITLE "compme AT-SPI fixture"
/* How long to wait for the fixture to log a key it received. Generous: a miss
 * costs the full wait, and a false "not received" would invert the verdict. */
#define KEY_WAIT_MS 2000
#define POLL_STEP_MS 20

static void sleep_ms(long ms) {
  struct timespec ts = {.tv_sec = ms / 1000, .tv_nsec = (ms % 1000) * 1000000L};
  nanosleep(&ts, NULL);
}

/* Number of "KEY <name>" lines in the fixture log. The fixture appends and
 * flushes per key, so a count is a stable observation point. */
static int count_key_lines(const char *path, const char *key_name) {
  FILE *file = fopen(path, "r");
  if (file == NULL) {
    return -1;
  }
  char line[256];
  char wanted[64];
  snprintf(wanted, sizeof(wanted), "KEY %s", key_name);
  int count = 0;
  while (fgets(line, sizeof(line), file) != NULL) {
    line[strcspn(line, "\n")] = '\0';
    if (strcmp(line, wanted) == 0) {
      count++;
    }
  }
  fclose(file);
  return count;
}

/* Wait until the fixture has logged `target` occurrences, or the budget runs
 * out. Returns the final count either way, so the caller reports what it saw. */
static int wait_for_key_count(const char *path, const char *key_name, int target) {
  long waited = 0;
  int count = count_key_lines(path, key_name);
  while (count < target && waited < KEY_WAIT_MS) {
    sleep_ms(POLL_STEP_MS);
    waited += POLL_STEP_MS;
    count = count_key_lines(path, key_name);
  }
  return count;
}

/* Depth-first search of the window tree for a window whose name matches.
 * Toolkits reparent and wrap, so the titled window is usually not a direct child
 * of the root. */
static Window find_window_by_name(Display *display, Window root, const char *name) {
  char *window_name = NULL;
  if (XFetchName(display, root, &window_name) && window_name != NULL) {
    int matched = strcmp(window_name, name) == 0;
    XFree(window_name);
    if (matched) {
      return root;
    }
  }
  Window parent;
  Window *children = NULL;
  unsigned int child_count = 0;
  if (!XQueryTree(display, root, &root, &parent, &children, &child_count)) {
    return None;
  }
  Window found = None;
  for (unsigned int i = 0; i < child_count && found == None; i++) {
    found = find_window_by_name(display, children[i], name);
  }
  if (children != NULL) {
    XFree(children);
  }
  return found;
}

/* Synthesize one Tab press+release through XTEST. The X server cannot tell
 * these from hardware keys, so they exercise passive grabs exactly as a real
 * keypress would. */
static void synthesize_tab(Display *display, KeyCode keycode) {
  XTestFakeKeyEvent(display, keycode, True, 0);
  XTestFakeKeyEvent(display, keycode, False, 0);
  XFlush(display);
}

/* Consume our grabbed KeyPress with `allow_mode` (AsyncKeyboard = swallow,
 * ReplayKeyboard = deliver to the focused app). Returns 0 when no grabbed
 * KeyPress arrived, which is itself a finding. */
static int resolve_grabbed_key(Display *display, int allow_mode) {
  long waited = 0;
  while (waited < KEY_WAIT_MS) {
    while (XPending(display) > 0) {
      XEvent event;
      XNextEvent(display, &event);
      if (event.type == KeyPress) {
        XAllowEvents(display, allow_mode, CurrentTime);
        XFlush(display);
        return 1;
      }
      if (event.type == KeyRelease) {
        XAllowEvents(display, allow_mode, CurrentTime);
        XFlush(display);
      }
    }
    sleep_ms(POLL_STEP_MS);
    waited += POLL_STEP_MS;
  }
  return 0;
}

int main(void) {
  const char *fixture_log = getenv("COMPME_ATSPI_FIXTURE_LOG");
  if (fixture_log == NULL) {
    fprintf(stderr, "keytap-spike: COMPME_ATSPI_FIXTURE_LOG is unset (run me through the harness)\n");
    return 2;
  }

  Display *display = XOpenDisplay(NULL);
  if (display == NULL) {
    fprintf(stderr, "keytap-spike: cannot open DISPLAY %s\n", getenv("DISPLAY"));
    return 2;
  }
  int event_base = 0;
  int error_base = 0;
  int major = 0;
  int minor = 0;
  if (!XTestQueryExtension(display, &event_base, &error_base, &major, &minor)) {
    fprintf(stderr, "keytap-spike: no XTEST extension; cannot synthesize keys\n");
    return 2;
  }

  Window root = DefaultRootWindow(display);
  Window fixture = find_window_by_name(display, root, FIXTURE_TITLE);
  if (fixture == None) {
    fprintf(stderr, "keytap-spike: fixture window '%s' not found\n", FIXTURE_TITLE);
    return 2;
  }
  /* Xvfb runs without a window manager, so nothing has assigned the input
   * focus: by default it is PointerRoot and keys follow the pointer. Set it
   * explicitly or the app receives nothing and every consume looks successful. */
  XSetInputFocus(display, fixture, RevertToParent, CurrentTime);
  XSync(display, False);

  KeyCode tab = XKeysymToKeycode(display, XK_Tab);
  if (tab == 0) {
    fprintf(stderr, "keytap-spike: no keycode for Tab in this layout\n");
    return 2;
  }

  int failures = 0;
  int expected = 0;

  /* A. Baseline: the rig can deliver a synthetic Tab to the application. */
  expected += 1;
  synthesize_tab(display, tab);
  int seen = wait_for_key_count(fixture_log, "Tab", expected);
  if (seen == expected) {
    printf("PASS keytap-baseline-ungrabbed-key-reaches-app\n");
  } else {
    printf("FAIL keytap-baseline-ungrabbed-key-reaches-app: app logged %d Tab(s), want %d\n", seen,
           expected);
    failures++;
  }

  /* Passive grab, keyboard frozen on match so we can decide per keystroke.
   * owner_events=False keeps the event on the grab window (ours) rather than
   * letting it reach the focused window first. */
  if (XGrabKey(display, tab, AnyModifier, root, False, GrabModeAsync, GrabModeSync) == BadAccess) {
    fprintf(stderr, "keytap-spike: another client already grabs Tab\n");
    return 2;
  }
  XSync(display, False);

  /* B. Consume: AsyncKeyboard resumes processing without replaying, so the key
   * must die with us. This is the accept path. */
  synthesize_tab(display, tab);
  if (!resolve_grabbed_key(display, AsyncKeyboard)) {
    printf("FAIL keytap-grab-delivers-key-to-interceptor: no KeyPress arrived while grabbed\n");
    failures++;
  } else {
    printf("PASS keytap-grab-delivers-key-to-interceptor\n");
  }
  sleep_ms(200); /* give a leaked key time to land before asserting it did not */
  seen = count_key_lines(fixture_log, "Tab");
  if (seen == expected) {
    printf("PASS keytap-consume-swallows-key-from-app\n");
  } else {
    printf("FAIL keytap-consume-swallows-key-from-app: app logged %d Tab(s), want %d\n", seen,
           expected);
    failures++;
  }

  /* C. Pass-through: ReplayKeyboard sends the frozen event to the focused
   * window as if the grab never existed. This is the "no suggestion visible,
   * Tab means Tab" path, and the reason a grab is not too invasive. */
  expected += 1;
  synthesize_tab(display, tab);
  if (!resolve_grabbed_key(display, ReplayKeyboard)) {
    printf("FAIL keytap-replay-delivers-key-to-interceptor: no KeyPress arrived while grabbed\n");
    failures++;
  }
  seen = wait_for_key_count(fixture_log, "Tab", expected);
  if (seen == expected) {
    printf("PASS keytap-replay-passes-key-through-to-app\n");
  } else {
    printf("FAIL keytap-replay-passes-key-through-to-app: app logged %d Tab(s), want %d\n", seen,
           expected);
    failures++;
  }

  /* D. Released: ungrabbing restores plain delivery, so the tap can be armed and
   * disarmed per suggestion without leaving the keyboard captured. */
  XUngrabKey(display, tab, AnyModifier, root);
  XSync(display, False);
  expected += 1;
  synthesize_tab(display, tab);
  seen = wait_for_key_count(fixture_log, "Tab", expected);
  if (seen == expected) {
    printf("PASS keytap-ungrab-restores-plain-delivery\n");
  } else {
    printf("FAIL keytap-ungrab-restores-plain-delivery: app logged %d Tab(s), want %d\n", seen,
           expected);
    failures++;
  }

  XCloseDisplay(display);
  if (failures > 0) {
    printf("VERDICT XGrabKey+XAllowEvents is NOT viable here (%d failed observation(s))\n",
           failures);
    return 1;
  }
  printf("VERDICT XGrabKey(GrabModeSync)+XAllowEvents gives per-keystroke consume/pass-through\n");
  return 0;
}
