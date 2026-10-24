"""Writes the initial HKEY_LOCAL_MACHINE hive of the root file system
(/etc/osyx/registry/machine.reg, the format of compatibility/win32/src/
registry.rs): the font substitutes that map the usual Windows font names
to the fonts OSyx ships in C:\\Windows\\Fonts (graphics/assets/fonts)."""
import sys

FONT_SUBSTITUTES = {
    "Arial": "Lato",
    "Helvetica": "Lato",
    "Segoe UI": "Lato",
    "Tahoma": "Lato",
    "Verdana": "Lato",
    "Calibri": "Lato",
    "Microsoft Sans Serif": "Lato",
    "MS Shell Dlg": "Lato",
    "MS Shell Dlg 2": "Lato",
    "Times New Roman": "KaTeX_Main",
    "Times": "KaTeX_Main",
    "Georgia": "KaTeX_Main",
    "Cambria": "KaTeX_Main",
    "Courier New": "KaTeX_Typewriter",
    "Courier": "KaTeX_Typewriter",
    "Consolas": "KaTeX_Typewriter",
    "Lucida Console": "KaTeX_Typewriter",
}

REG_SZ = 1


def hexs(b: bytes) -> str:
    return b.hex()


def main() -> None:
    key = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\FontSubstitutes"
    parts = key.split("\\")
    lines = []
    for i in range(1, len(parts) + 1):
        lines.append("K\t" + hexs("\\".join(parts[:i]).encode()))
    for name, target in FONT_SUBSTITUTES.items():
        data = (target + "\0").encode("utf-16-le")
        lines.append("V\t%s\t%s\t%d\t%s" % (hexs(key.encode()), hexs(name.encode()), REG_SZ, hexs(data)))
    if "--zen" in sys.argv[2:]:
        def value(path: str, name: str, text: str) -> None:
            segments = path.split("\\")
            for i in range(1, len(segments) + 1):
                record = "K\t" + hexs("\\".join(segments[:i]).encode())
                if record not in lines:
                    lines.append(record)
            lines.append("V\t%s\t%s\t%d\t%s" % (
                hexs(path.encode()), hexs(name.encode()), REG_SZ,
                hexs((text + "\0").encode("utf-16-le"))))

        command = r'"C:\system\bin\zen" "%1"'
        classes = r"SOFTWARE\Classes"
        for scheme in ("http", "https"):
            value(classes + "\\" + scheme, "", "URL:Zen Browser")
            value(classes + "\\" + scheme, "URL Protocol", "")
            value(classes + "\\" + scheme + r"\shell\open\command", "", command)
        value(classes + r"\ZenHTML", "", "Zen HTML Document")
        value(classes + r"\ZenHTML\shell\open\command", "", command)
        for extension in (".htm", ".html", ".xhtml"):
            value(classes + "\\" + extension, "", "ZenHTML")
        client = r"SOFTWARE\Clients\StartMenuInternet"
        value(client, "", "Zen")
        value(client + r"\Zen", "", "Zen Browser")
        value(client + r"\Zen\shell\open\command", "", r'"C:\system\bin\zen"')
        caps = client + r"\Zen\Capabilities"
        value(caps, "ApplicationName", "Zen Browser")
        value(caps, "ApplicationDescription", "Browse the web with Zen")
        for scheme in ("http", "https"):
            value(caps + r"\URLAssociations", scheme, scheme)
        for extension in (".htm", ".html", ".xhtml"):
            value(caps + r"\FileAssociations", extension, "ZenHTML")
        value(r"SOFTWARE\RegisteredApplications", "Zen Browser", caps)
    with open(sys.argv[1], "w", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
