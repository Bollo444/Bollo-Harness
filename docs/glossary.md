# Glossary

Everyday definitions for the terms you'll see in this project. If something isn't here, ask in the chat.

| Term | What it means (not jargon) |
|---|---|
| **ADR** | A decision we wrote down so we don't have to re-argue it later. The full name is "Architecture Decision Record", but you can just think of it as "the rule we all agreed on." |
| **OD** | A choice waiting on the person who owns this project. Like a sticky note on their desk that says "ask me about this before we change it." |
| **Containment** | Running a program inside a locked room so it can't mess with the rest of your computer. Think of it like putting a sandbox inside a playpen — the sandbox can't get out and step on things. |
| **TUI** | A screen that lives inside the terminal window. It's like a regular app window but made of text characters instead of pictures. You move around with arrow keys, not a mouse. |
| **Offscreen render** | Taking a picture of the screen without needing a real person to look at it. The computer draws it into memory, and then we can check that the right letters are in the right places. Like a screenshot that the computer reads automatically. |
| **Golden test** | A test where we compare the output to a known-good version we saved earlier. If the output changed, the test fails and we notice. It's like a recipe where if the cookies don't look exactly right, we know something changed. |
| **Closure count** | How many extra helper packages get pulled in when you add one new package. Measured by `cargo tree -e normal` — like if you buy one LEGO set and it comes with 64 extra pieces you didn't plan on. |
| **Journal** | A notebook the program writes to as it works, so if it crashes later it can look back and see what happened. It's one file (a SQLite database) that records each step. |
| **Approval** | A pause the program hits when it wants to do something that might be risky. It asks the human "are you sure?" before going ahead. Like a seatbelt warning buzzer. |
| **Headless** | Running a program without a screen or keyboard attached. The program still works, but it can't open a window or ask you questions — it just does its job and reports back. |