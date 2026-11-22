# Accessibility Statement — DRAFT (needs legal review)

> Not in force. Describes the current state; publish only after review.

## Conformance
Daybook targets **WCAG 2.2 Level AA**. This is the release floor; WCAG AAA is
advisory, not claimed. Contrast pairs, keyboard operation, focus indicators,
semantic landmarks and reduced-motion support are implemented and tested.

## Measures
- Skip link and single `<main>` landmark on every page.
- Visible `:focus-visible` indicators; keyboard-operable controls and native
  `<details>`/`<summary>` disclosures.
- Status conveyed in text as well as color; `aria-current` on the active nav
  item; ARIA on collapsible groups.
- `prefers-reduced-motion` respected; no theme flash on load.
- Mobile-first layout with 44px touch targets on narrow screens.

## Known gaps
- No automated screen-reader or real-device audit has been run; verification
  is static/markup-based plus developer smoke tests.
- No formal EAA conformance assessment has been commissioned.

## Feedback
Report accessibility problems privately to the maintainer. Fixes are tracked
like any other defect.
