/*
 * AT-SPI2 client probe for the Linux harness (ROADMAP Phase 2.7).
 *
 * Asserts that the capabilities Phase 2.1-2.4 need are actually available in
 * this session, against linux-atspi-fixture.c:
 *   - the fixture application is visible on the accessibility desktop
 *   - its named field exposes the Text interface (read_context)
 *   - the caret offset is readable (caret tracking)
 *   - per-character screen extents are readable (caret_rect / text_range_rect)
 *   - EditableText can insert at an offset and the change reads back (insert)
 *
 * It is written against libatspi — the same C API the Rust `atspi` crate wraps —
 * so a green run is evidence about the stack the real adapter will use, not
 * about a binding. It is a harness tool, not product code: `platform_linux`
 * still owns the adapter, and this file is compiled on demand by
 * run-linux-atspi-session.sh.
 *
 * Exit codes: 0 ok · 2 fixture/field not found · 3 missing interface ·
 * 4 readback mismatch · 5 AT-SPI init failed.
 */
#include <atspi/atspi.h>
#include <stdio.h>
#include <string.h>

#define DEFAULT_APP "compme-fixture"
#define DEFAULT_FIELD "compme-fixture-entry"
/* Inserted at offset 0, so the readback must start with it. Deliberately not a
 * substring of the fixture's own text, or a no-op insert would still "pass". */
#define INSERT_TEXT "zz "
/* AT-SPI recursion is over a live UI tree; bound it so a pathological or
 * looping hierarchy fails loudly instead of hanging the harness. */
#define MAX_DEPTH 12

static int warn_error(const char *what, GError **error) {
  if (*error != NULL) {
    fprintf(stderr, "probe: %s: %s\n", what, (*error)->message);
    g_clear_error(error);
    return 1;
  }
  return 0;
}

/* Print one line per child of `node` (name + role). A "not found" failure is
 * otherwise indistinguishable from "the bridge never registered the app", which
 * is the single most common way this harness breaks. */
static void describe_children(AtspiAccessible *node, const char *label) {
  GError *error = NULL;
  gint children = atspi_accessible_get_child_count(node, &error);
  if (warn_error("get_child_count", &error)) {
    return;
  }
  fprintf(stderr, "probe: %s has %d child(ren):\n", label, children);
  for (gint i = 0; i < children; i++) {
    AtspiAccessible *child = atspi_accessible_get_child_at_index(node, i, &error);
    if (warn_error("get_child_at_index", &error) || child == NULL) {
      continue;
    }
    char *name = atspi_accessible_get_name(child, &error);
    warn_error("get_name", &error);
    AtspiRole role = atspi_accessible_get_role(child, &error);
    warn_error("get_role", &error);
    char *role_name = atspi_role_get_name(role);
    fprintf(stderr, "probe:   [%d] name=%s role=%s\n", i, name != NULL ? name : "(null)",
            role_name != NULL ? role_name : "(null)");
    g_free(name);
    g_free(role_name);
    g_clear_object(&child);
  }
}

/* Depth-first search for an accessible whose name matches `name`. Returns a new
 * reference, or NULL. */
static AtspiAccessible *find_named(AtspiAccessible *node, const char *name, int depth) {
  if (node == NULL || depth > MAX_DEPTH) {
    return NULL;
  }
  GError *error = NULL;
  char *node_name = atspi_accessible_get_name(node, &error);
  warn_error("get_name", &error);
  if (node_name != NULL && strcmp(node_name, name) == 0) {
    g_free(node_name);
    return g_object_ref(node);
  }
  g_free(node_name);

  gint children = atspi_accessible_get_child_count(node, &error);
  if (warn_error("get_child_count", &error)) {
    return NULL;
  }
  for (gint i = 0; i < children; i++) {
    AtspiAccessible *child = atspi_accessible_get_child_at_index(node, i, &error);
    if (warn_error("get_child_at_index", &error)) {
      continue;
    }
    AtspiAccessible *found = find_named(child, name, depth + 1);
    g_clear_object(&child);
    if (found != NULL) {
      return found;
    }
  }
  return NULL;
}

int main(int argc, char **argv) {
  const char *app_name = argc > 1 ? argv[1] : DEFAULT_APP;
  const char *field_name = argc > 2 ? argv[2] : DEFAULT_FIELD;

  if (atspi_init() != 0) {
    fprintf(stderr, "probe: atspi_init failed (is the a11y bus running?)\n");
    return 5;
  }

  GError *error = NULL;
  AtspiAccessible *desktop = atspi_get_desktop(0);
  if (desktop == NULL) {
    fprintf(stderr, "probe: no accessibility desktop\n");
    return 5;
  }

  AtspiAccessible *app = find_named(desktop, app_name, 0);
  if (app == NULL) {
    fprintf(stderr, "probe: application %s not on the accessibility desktop\n", app_name);
    describe_children(desktop, "desktop");
    return 2;
  }
  AtspiAccessible *field = find_named(app, field_name, 0);
  if (field == NULL) {
    fprintf(stderr, "probe: field %s not found under %s\n", field_name, app_name);
    describe_children(app, app_name);
    return 2;
  }

  AtspiRole role = atspi_accessible_get_role(field, &error);
  warn_error("get_role", &error);
  char *role_name = atspi_role_get_name(role);

  AtspiText *text = atspi_accessible_get_text_iface(field);
  if (text == NULL) {
    fprintf(stderr, "probe: %s exposes no Text interface\n", field_name);
    return 3;
  }
  char *value = atspi_text_get_text(text, 0, -1, &error);
  if (warn_error("get_text", &error) || value == NULL) {
    return 3;
  }
  gint caret = atspi_text_get_caret_offset(text, &error);
  warn_error("get_caret_offset", &error);
  AtspiRect *extents = atspi_text_get_character_extents(text, 0, ATSPI_COORD_TYPE_SCREEN, &error);
  if (warn_error("get_character_extents", &error) || extents == NULL) {
    fprintf(stderr, "probe: no character extents (caret geometry unavailable)\n");
    return 3;
  }
  /* A zero-area rect means the toolkit answered but has no usable geometry —
   * that would silently defeat overlay placement, so treat it as a failure. */
  if (extents->width <= 0 || extents->height <= 0) {
    fprintf(stderr, "probe: degenerate character extents %dx%d\n", extents->width,
            extents->height);
    return 3;
  }

  printf("role\t%s\n", role_name);
  printf("text\t%s\n", value);
  printf("caret\t%d\n", caret);
  printf("extents\t%d,%d,%d,%d\n", extents->x, extents->y, extents->width, extents->height);

  AtspiEditableText *editable = atspi_accessible_get_editable_text_iface(field);
  if (editable == NULL) {
    fprintf(stderr, "probe: %s exposes no EditableText interface\n", field_name);
    return 3;
  }
  if (!atspi_editable_text_insert_text(editable, 0, INSERT_TEXT, (gint)strlen(INSERT_TEXT),
                                       &error) ||
      error != NULL) {
    warn_error("insert_text", &error);
    fprintf(stderr, "probe: EditableText insert refused\n");
    return 3;
  }
  char *after = atspi_text_get_text(text, 0, -1, &error);
  if (warn_error("get_text after insert", &error) || after == NULL) {
    return 3;
  }
  if (strncmp(after, INSERT_TEXT, strlen(INSERT_TEXT)) != 0) {
    fprintf(stderr, "probe: insert did not read back: %s\n", after);
    return 4;
  }
  printf("inserted\t%s\n", after);

  g_free(value);
  g_free(after);
  g_free(role_name);
  g_free(extents);
  g_clear_object(&field);
  g_clear_object(&app);
  printf("PROBE_OK\n");
  return 0;
}
