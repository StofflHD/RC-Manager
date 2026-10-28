#!/usr/bin/env python3
"""Patches FreeRDP's Windows client (wfreerdp) for RC Manager.

wfreerdp asks with its own message boxes (certificate) and the Windows credential dialog when
stdin is not a console. RC Manager answers these questions in its own dialogs on stdin/stdout,
like FreeRDP's X11 client on Linux, and keeps trusted certificates itself. With the environment
variable RC_MANAGER_PROMPTS set (only RC Manager sets it), wfreerdp therefore uses the console
questions of the common client code. Without it, wfreerdp behaves as usual.

Usage: patch-freerdp.py <FreeRDP source directory>
"""
import pathlib
import sys

src = pathlib.Path(sys.argv[1]) / "client" / "Windows" / "wf_client.c"
text = src.read_text(encoding="utf-8")

PATCHES = [
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
]

for old, new in PATCHES:
    if text.count(old) != 1:
        sys.exit(f"patch-freerdp: expected text not found exactly once in {src}:\n{old}")
    text = text.replace(old, new)

src.write_text(text, encoding="utf-8", newline="\n")
print(f"patched {src}")
