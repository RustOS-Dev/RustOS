/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * xhello: a minimal X11 client for checking Xwayland. Opens a 400x300
 * window titled "xhello", fills it with a solid colour on every Expose and
 * prints "xhello: mapped" once the server has shown it.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xcb/xcb.h>

int main(void)
{
	int screen_num;
	xcb_connection_t *c = xcb_connect(NULL, &screen_num);
	if (xcb_connection_has_error(c)) {
		fprintf(stderr, "xhello: cannot connect to the X server\n");
		return 1;
	}
	const xcb_setup_t *setup = xcb_get_setup(c);
	xcb_screen_iterator_t it = xcb_setup_roots_iterator(setup);
	for (int i = 0; i < screen_num; i++)
		xcb_screen_next(&it);
	xcb_screen_t *screen = it.data;
	printf("xhello: connected, vendor %.*s, screen %ux%u\n",
	       xcb_setup_vendor_length(setup), xcb_setup_vendor(setup),
	       screen->width_in_pixels, screen->height_in_pixels);

	xcb_window_t win = xcb_generate_id(c);
	uint32_t values[2] = { 0x2060c0, XCB_EVENT_MASK_EXPOSURE | XCB_EVENT_MASK_KEY_PRESS };
	xcb_create_window(c, XCB_COPY_FROM_PARENT, win, screen->root, 0, 0, 400, 300, 0,
			  XCB_WINDOW_CLASS_INPUT_OUTPUT, screen->root_visual,
			  XCB_CW_BACK_PIXEL | XCB_CW_EVENT_MASK, values);
	xcb_change_property(c, XCB_PROP_MODE_REPLACE, win, XCB_ATOM_WM_NAME, XCB_ATOM_STRING, 8,
			    strlen("xhello"), "xhello");
	xcb_map_window(c, win);
	xcb_flush(c);

	int mapped = 0;
	xcb_generic_event_t *ev;
	while ((ev = xcb_wait_for_event(c))) {
		switch (ev->response_type & 0x7f) {
		case XCB_EXPOSE:
			xcb_clear_area(c, 0, win, 0, 0, 0, 0);
			xcb_flush(c);
			if (!mapped) {
				mapped = 1;
				printf("xhello: mapped\n");
				fflush(stdout);
			}
			break;
		case XCB_KEY_PRESS:
			printf("xhello: key %u\n", ((xcb_key_press_event_t *)ev)->detail);
			fflush(stdout);
			break;
		}
		free(ev);
	}
	return 0;
}
