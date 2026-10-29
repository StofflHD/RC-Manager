#!/usr/bin/env python3
"""Patches FreeRDP's Windows client (wfreerdp) for RC Manager.

wfreerdp asks with its own message boxes (certificate) and the Windows credential dialog when
stdin is not a console. RC Manager answers these questions in its own dialogs on stdin/stdout,
like FreeRDP's X11 client on Linux, and keeps trusted certificates itself. With the environment
variable RC_MANAGER_PROMPTS set (only RC Manager sets it), wfreerdp therefore uses the console
questions of the common client code. Without it, wfreerdp behaves as usual.

Embedded (/parent-window), wfreerdp's keyboard hook only forwards keys while the parent window
has the focus, but mouse and keyboard input stay suspended until its own window gets the focus –
so no window could receive both. The hook therefore also accepts its own window.

wfreerdp sets a new mouse pointer from the RDP server (e.g. the hand over a link) with SetCursor
on its network thread, which Windows ignores; the pointer only changed with the next mouse move.
It now asks its window's thread to refresh the pointer right away.

Embedded, wfreerdp shares its input queue with RC Manager's thread, which can take the wake-up
for new input; wfreerdp then only looked at its messages after its 5 s wait timed out (input
stalled, and RC Manager spun on the pending input). Embedded, it now waits at most 50 ms.

Usage: patch-freerdp.py <FreeRDP source directory>
"""
import pathlib
import sys

client = pathlib.Path(sys.argv[1]) / "client" / "Windows"

PATCHES = {
    "wf_client.c": [
        (
            "static BOOL wf_has_console(void)\n{\n",
            "static BOOL wf_has_console(void)\n{\n"
            "\t/* RC Manager: questions on stdin/stdout instead of message boxes */\n"
            "\tif (getenv(\"RC_MANAGER_PROMPTS\"))\n"
            "\t\treturn TRUE;\n",
        ),
        (
            "\tif (freerdp_settings_get_bool(instance->context->settings, FreeRDP_CredentialsFromStdin))\n"
            "\t\treturn client_cli_authenticate_ex(instance, username, password, domain, reason);\n",
            "\tif (freerdp_settings_get_bool(instance->context->settings, FreeRDP_CredentialsFromStdin) ||\n"
            "\t    getenv(\"RC_MANAGER_PROMPTS\"))\n"
            "\t\treturn client_cli_authenticate_ex(instance, username, password, domain, reason);\n",
        ),
        (
            "\t\tDWORD status = MsgWaitForMultipleObjectsEx(nCount, handles, 5 * 1000, QS_ALLINPUT,\n",
            "\t\t/* RC Manager: embedded, the input queue is shared with the parent's thread, which can\n"
            "\t\t * take the wake-up for new input; check the messages at least every 50 ms. */\n"
            "\t\tconst DWORD rcm_wait =\n"
            "\t\t    freerdp_settings_get_bool(settings, FreeRDP_EmbeddedWindow) ? 50 : 5 * 1000;\n"
            "\t\tDWORD status = MsgWaitForMultipleObjectsEx(nCount, handles, rcm_wait, QS_ALLINPUT,\n",
        ),
    ],
    "wf_graphics.c": [
        (
            "\t\tSetCursor(hCur);\n"
            "\t\twfc->cursor = hCur;\n",
            "\t\tSetCursor(hCur);\n"
            "\t\twfc->cursor = hCur;\n"
            "\t\t/* RC Manager: SetCursor only works on the window's thread; refresh it there */\n"
            "\t\tPOINT rcm_pt;\n"
            "\t\tif (wfc->hwnd && GetCursorPos(&rcm_pt) && (WindowFromPoint(rcm_pt) == wfc->hwnd))\n"
            "\t\t\tPostMessage(wfc->hwnd, WM_SETCURSOR, (WPARAM)wfc->hwnd,\n"
            "\t\t\t            MAKELPARAM(HTCLIENT, WM_MOUSEMOVE));\n",
        ),
    ],
    "wf_event.c": [
        (
            "\t\tif (gui_thread_info.hwndFocus != wfc->hWndParent)\n",
            "\t\t/* RC Manager: keys also while wfreerdp's own (embedded) window has the focus */\n"
            "\t\tif (gui_thread_info.hwndFocus != wfc->hWndParent &&\n"
            "\t\t    gui_thread_info.hwndFocus != g_main_hWnd)\n",
        ),
    ],
}

for name, patches in PATCHES.items():
    src = client / name
    text = src.read_text(encoding="utf-8")
    for old, new in patches:
        if text.count(old) != 1:
            sys.exit(f"patch-freerdp: expected text not found exactly once in {src}:\n{old}")
        text = text.replace(old, new)
    src.write_text(text, encoding="utf-8", newline="\n")
    print(f"patched {src}")
