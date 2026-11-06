#!/usr/bin/env python3
"""Tiny helper to read/write Recon's webview localStorage (WebKit stores values as UTF-16LE).

The app's webview keeps tab and diagram preferences in the real user's WebKit
storage (setting HOME does not redirect it), so capture.sh uses this to
(1) drop demo-only keys, and (2) set the diagram preference for the screenshot
and restore the user's own value afterwards.

Usage:
  localstorage.py purge-demo
  localstorage.py get KEY        # prints the value, or nothing if the key is absent
  localstorage.py set KEY VALUE
  localstorage.py delete KEY
"""
import glob, os, sqlite3, sys

PATTERN = os.path.expanduser(
    "~/Library/WebKit/com.recon.app/WebsiteData/Default/*/*/LocalStorage/localstorage.sqlite3")

def dbs():
    return glob.glob(PATTERN)

def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    for path in dbs():
        con = sqlite3.connect(path)
        if cmd == "purge-demo":
            con.execute("DELETE FROM ItemTable WHERE key LIKE '%demo-connection%'")
        elif cmd == "get":
            row = con.execute("SELECT value FROM ItemTable WHERE key=?", (sys.argv[2],)).fetchone()
            if row is not None:
                sys.stdout.write(bytes(row[0]).decode("utf-16le"))
        elif cmd == "set":
            con.execute("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)",
                        (sys.argv[2], sys.argv[3].encode("utf-16le")))
        elif cmd == "delete":
            con.execute("DELETE FROM ItemTable WHERE key=?", (sys.argv[2],))
        else:
            sys.exit(__doc__)
        con.commit()
        con.close()

if __name__ == "__main__":
    main()
