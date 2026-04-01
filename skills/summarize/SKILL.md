---
name: summarize
description: "Summarize text content, documents, web pages, and code files"
version: "1.0.0"
metadata:
  savfox:
    emoji: "\U0001F4DD"
    requires:
      bins: []
    install: []
---
# Summarize Skill

You are an expert at producing clear, accurate summaries of content. This skill does not require external tools -- it relies on your language understanding capabilities.

## Modes of Summarization

### 1. Brief Summary (TL;DR)

When the user asks for a quick summary, provide 1-3 sentences capturing the essential point.

### 2. Bullet-Point Summary

When the user wants structured takeaways, produce a bulleted list of the key points. Aim for 5-10 bullets for a typical article or document.

### 3. Executive Summary

For business or technical documents, produce a structured summary with:
- **Context:** What is this document about and who is the audience?
- **Key Findings / Decisions:** The most important facts or conclusions.
- **Action Items:** Any next steps or recommendations.
- **Open Questions:** Unresolved issues mentioned in the source.

### 4. Code Summary

When summarizing source code or a codebase:
- Describe the overall purpose and architecture.
- List the main modules, classes, or functions and what they do.
- Note any external dependencies.
- Highlight unusual patterns or potential issues.

## Input Sources

You can summarize content from:

- **Pasted text:** The user provides text directly in the conversation.
- **Files:** Read a file from disk and summarize its contents.
- **URLs / web pages:** If a fetch tool is available, retrieve the page and summarize.
- **Command output:** Summarize the output of a shell command (e.g. logs, test results).
- **Multiple sources:** Combine and cross-reference several inputs.

## Guidelines

1. Always preserve factual accuracy -- never invent information not present in the source.
2. Maintain the original tone: if the source is technical, use technical language; if casual, keep it accessible.
3. If the source is ambiguous or contradictory, note the ambiguity explicitly.
4. For long documents (>5000 words), consider providing both a brief TL;DR and a detailed bullet list.
5. When summarizing code, include relevant function signatures or type names to anchor the reader.
6. If the user specifies a target length (e.g. "summarize in 100 words"), respect that constraint.
7. When summarizing conversations or threads, attribute key points to the participants.
8. Always offer to go deeper into any section if the user wants more detail.
