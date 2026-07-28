/*
 * GTK3 accessibility fixture for the Linux AT-SPI2 harness (ROADMAP Phase 2.7).
 *
 * A deterministic target for the AT-SPI2 read/insert paths that Phase 2.1-2.4
 * will implement: one single-line GtkEntry and one multi-line GtkTextView, each
 * carrying a fixed accessible name so a probe can find it without guessing at
 * widget hierarchy. Runs under Xvfb; no desktop environment required.
 *
 * Deliberately not a Rust fixture: this exists to exercise a *real* GTK/ATK
 * accessibility stack (the same one shipped apps use), and pulling gtk-rs into
 * the workspace to do it would add a large dependency tree for a test double.
 * Built on demand by run-linux-atspi-session.sh; nothing links it into compme.
 *
 * Prints "FIXTURE_READY" once the window is mapped so the harness can wait for
 * an event instead of sleeping and hoping.
 */
#include <gtk/gtk.h>
#include <stdio.h>

/* The names the probe matches on. Keep in sync with linux-atspi-probe.c and the
 * harness defaults. */
#define FIXTURE_PRGNAME "compme-fixture"
#define ENTRY_A11Y_NAME "compme-fixture-entry"
#define VIEW_A11Y_NAME "compme-fixture-textview"
/* A misspelling on purpose: the grammar-fix path's canonical example, so a
 * future correction test has something to correct. */
#define ENTRY_TEXT "teh quick brown"
#define VIEW_TEXT "Hello from the compme AT-SPI2 fixture.\nSecond line."

static void name_accessible(GtkWidget *widget, const char *name) {
  AtkObject *accessible = gtk_widget_get_accessible(widget);
  if (accessible != NULL) {
    atk_object_set_name(accessible, name);
  }
}

static void on_map(GtkWidget *widget, gpointer user_data) {
  (void)widget;
  (void)user_data;
  /* stdout is a pipe under the harness, so it is block-buffered: flush or the
   * reader blocks forever waiting for a line that is sitting in libc. */
  printf("FIXTURE_READY\n");
  fflush(stdout);
}

/* Log every key the application actually receives. This is how the accept-key
 * spike tells "the interceptor swallowed the key" from "the interceptor saw it
 * and the app got it anyway" — the whole question for a Tab-to-accept tap.
 * Returns FALSE so normal widget handling still runs. */
static gboolean on_key_press(GtkWidget *widget, GdkEventKey *event, gpointer user_data) {
  (void)widget;
  (void)user_data;
  const char *name = gdk_keyval_name(event->keyval);
  printf("KEY %s\n", name != NULL ? name : "unknown");
  fflush(stdout);
  return FALSE;
}

int main(int argc, char **argv) {
  /* The AT-SPI application name is taken from the program name, so set it
   * before gtk_init registers with the accessibility bus. */
  g_set_prgname(FIXTURE_PRGNAME);
  gtk_init(&argc, &argv);

  GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
  gtk_window_set_title(GTK_WINDOW(window), "compme AT-SPI fixture");
  gtk_window_set_default_size(GTK_WINDOW(window), 480, 240);
  g_signal_connect(window, "destroy", G_CALLBACK(gtk_main_quit), NULL);
  g_signal_connect_after(window, "map", G_CALLBACK(on_map), NULL);
  /* Not `_after`: the toplevel's default handler forwards the event to the focus
   * widget, so connecting before it is what sees Tab and Escape too. */
  g_signal_connect(window, "key-press-event", G_CALLBACK(on_key_press), NULL);

  GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 8);
  gtk_container_set_border_width(GTK_CONTAINER(box), 8);
  gtk_container_add(GTK_CONTAINER(window), box);

  GtkWidget *entry = gtk_entry_new();
  gtk_entry_set_text(GTK_ENTRY(entry), ENTRY_TEXT);
  name_accessible(entry, ENTRY_A11Y_NAME);
  gtk_box_pack_start(GTK_BOX(box), entry, FALSE, FALSE, 0);

  GtkWidget *view = gtk_text_view_new();
  GtkTextBuffer *buffer = gtk_text_view_get_buffer(GTK_TEXT_VIEW(view));
  gtk_text_buffer_set_text(buffer, VIEW_TEXT, -1);
  name_accessible(view, VIEW_A11Y_NAME);
  gtk_box_pack_start(GTK_BOX(box), view, TRUE, TRUE, 0);

  gtk_widget_show_all(window);
  /* Focus the entry so its caret offset is meaningful to the probe. */
  gtk_widget_grab_focus(entry);
  gtk_main();
  return 0;
}
