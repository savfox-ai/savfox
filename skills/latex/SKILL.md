---
name: latex
description: Create and compile LaTeX documents for academic and technical writing.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📄"
    requires:
      bins:
        - pdflatex
      env: []
    install:
      - id: brew
        kind: brew
        formula: basictex
        bins: [pdflatex]
        label: Homebrew (BasicTeX)
      - id: apt
        kind: apt
        package: texlive-base
        bins: [pdflatex]
        label: APT
---

# LaTeX Skill

Create and compile LaTeX documents.

## Compile

```bash
pdflatex document.tex
```

With BibTeX:
```bash
pdflatex document.tex && bibtex document && pdflatex document.tex && pdflatex document.tex
```

## Document Template

```latex
\documentclass{article}
\usepackage[utf8]{inputenc}
\usepackage{amsmath}

\title{My Document}
\author{Author Name}
\date{\today}

\begin{document}
\maketitle

\section{Introduction}
Your text here.

\end{document}
```

## Math

Inline: `$E = mc^2$`

Display:
```latex
\begin{equation}
  \int_0^\infty e^{-x^2} dx = \frac{\sqrt{\pi}}{2}
\end{equation}
```

## Guidelines

- Run `pdflatex` twice for cross-references
- Use `latexmk -pdf` for automatic dependency tracking
- Use `\usepackage{hyperref}` for clickable links in PDF
