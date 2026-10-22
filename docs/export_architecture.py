#!/usr/bin/env python3
"""Export the canonical architecture Markdown and figures to the DOCX white paper."""

import argparse
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pandoc", default="pandoc", help="Pandoc 3.x executable")
    args = parser.parse_args()
    pandoc = shutil.which(args.pandoc)
    if pandoc is None:
        parser.error("Pandoc is required; install it or supply --pandoc PATH")

    docs = Path(__file__).resolve().parent
    source = docs / "ARCHITECTURE.md"
    output = docs / "Lattice-DRTOS-Architecture-White-Paper.docx"
    heading, body = source.read_text(encoding="utf-8").split("\n", 1)
    if not heading.startswith("# "):
        parser.error("ARCHITECTURE.md must begin with its document title")

    # Generate fully before replacing the export, without depending on its old copy.
    with tempfile.TemporaryDirectory(prefix="lattice-architecture-") as temporary:
        candidate = Path(temporary) / output.name
        subprocess.run(
            [
                pandoc, "--from=gfm", "--to=docx", "--standalone",
                f"--metadata=title:{heading[2:]}",
                f"--resource-path={docs}", f"--output={candidate}",
            ],
            input=body, text=True, check=True, cwd=docs,
        )
        shutil.copyfile(candidate, output)
    print(output)


if __name__ == "__main__":
    main()
