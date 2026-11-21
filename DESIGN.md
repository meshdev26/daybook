# Daybook — Design System

## Perspectives
This design system is informed by two lenses:
- **User experience**: usability, accessibility, interaction patterns, user research.
- **Design leadership**: brand coherence, product strategy, design systems at scale.

---

## 1. User Experience View

### User research insights
- Sellers are often non-technical; setup must be < 5 minutes.
- Buyers want instant access after payment; friction kills conversion.
- Mobile-first: most Telegram users are on phones.

### Usability principles
- **One primary action per screen** (Fitts's Law).
- **Progressive disclosure**: show basics first, advanced on demand.
- **Forgiving**: confirm destructive actions; allow undo where possible.
- **Feedback**: every action gets immediate visual confirmation.
- **Empty states**: guide users when there's no data.

### Accessibility (WCAG 2.2 AA)
- Contrast ratio ≥ 4.5:1 for text.
- Keyboard navigable; visible focus rings.
- Semantic HTML (headings, landmarks, labels).
- Screen-reader friendly (aria labels).
- Reduced motion support (`prefers-reduced-motion`).

### Interaction patterns
- Forms: inline validation, clear errors.
- Lists: swipe actions on mobile, hover on desktop.
- Modals: only for destructive confirmations.
- Loading: skeleton screens, not spinners where possible.

---

## 2. Design Leadership View

### Brand identity
- **Name**: Telegram Business Platform.
- **Tone**: calm, confident, helpful — never loud.
- **Voice**: clear, concise, human. Avoid jargon.

### Design system at scale
- Single source of truth: tokens (color, spacing, type).
- Component library shared between bot (inline keyboards) and web.
- Theming: light (default), dark (shipped — `[data-theme="dark"]` plus
  `prefers-color-scheme` auto).

### Product strategy
- **North star**: time-to-first-sale < 10 minutes.
- **Activation**: guided onboarding wizard.
- **Retention**: analytics, scheduled posts, customer insights.
- **Growth**: referral codes, marketplace discovery.

### Design ops
- Design tokens in CSS variables.
- Component documentation in `DESIGN.md`.
- Review process: every UI change reviewed against principles.

---

## 3. Design Philosophy (operator's ledger)
Calm, confident, helpful — never loud. One idea carries the identity: the
operator's ledger — a commerce back-office set like a well-kept financial
book. Borders lead, shadows whisper, figures sit in mono, status is a dot
before a word. No hover-lift theatre, no gradient washes. Generous
whitespace, precise typography, subtle depth, single clear action per screen.

**Brand:** the product is called **Daybook** — a "daybook" is the daily account book itself, so the name makes the ledger metaphor callable; display text only, no token or path changes.

## 4. Tokens

### Typography (UX + design leadership)
- **Font stack (UI)**: `"Plex Sans", -apple-system, "Segoe UI", "Helvetica Neue", sans-serif` (bundled variable woff2, latin subset). **Data/mono**: `"Plex Mono", ui-monospace, "SF Mono", Menlo, monospace` for figures, ids, and code. No system-font-only claim: the operator console ships bundled Plex fonts from the binary (no CDN).
- **Design rationale**: bundled Plex fonts keep the ledger aesthetic consistent across OSes; served from the binary (no CDN, no FOUT beyond `font-display: swap`).
- **UX rationale**: Plex Sans/Mono are highly legible at small sizes; mono figures aid scanning amounts and ids.
- **Type scale** (modular 1.25):
  - Display: 34px / 600 / 1.1
  - H1: 21px / 600 / 1.2
  - H2: 16px / 600 / 1.25
  - H3: 14px / 600 / 1.3
  - Body: 14.5px / 400 / 1.5
  - Small: 13px / 400 / 1.4
  - Caption: 11px / 500 / 1.3 (uppercase, tracked)
- **Weights**: 400 (regular), 500 (medium), 600 (semibold). Avoid 700+ for calm feel.
- **Line length**: max 65ch for readability.
- **Letter spacing**: -0.02em for headings, 0 for body, 0.05em for captions.

### Color (ledger-green; tokens in `assets/app.css`)
Light:
- Background (paper): `#F4F6F3` (`--bg`)
- Surface: `#FFFFFF` (`--surface`); recessed `#FAFBF9` (`--surface-2`)
- Text primary (ink): `#17201C` (`--ink`)
- Text secondary: `#57645C` (`--ink-2`); faint `#5F6B63` (`--ink-3`)
- Brand (deep billiard green): `#1E6B52` (`--brand`); hover `#175542`; ink `#F1F7F3`
- Status: ok `#1E6B52`; warn `#8A5A12`; danger (brick) `#A6392F`
- Border (hairline): `#DEE3DC` (`--line`); strong `#8A958D` (`--line-strong`)
Dark (`[data-theme="dark"]` / `prefers-color-scheme: dark`): background `#101512`, surface `#171D19`, ink `#E8EDE8`, brand `#4FBE8F`, warn `#D9A94F`, danger `#E07A6E`. All pairs WCAG 2.2 AA (checked in `app.css` header). The token contract uses Radix 12-step role semantics over these unchanged ledger-green values: steps 1–3 backgrounds, 6–7 borders/control edges, 9–10 solid fills, 11–12 text. WCAG 2.2 AA is the single release floor; AAA is advisory only.

### Spacing (8px grid)
xs 4, sm 8, md 16, lg 24, xl 32, xxl 48.

## 5. Components
- **Card/panel**: white surface, 10px radius, 1px hairline border, 20px padding, no shadow.
- **Button primary**: brand fill, brand-ink text, 7px radius.
- **Button secondary**: surface fill, ink text, hairline border.
- **Input**: 7px radius, 1px border (strong), focus ring brand.
- **Nav**: minimal text links; active = brand tint.
- **Table**: subtle dividers, generous padding.
- **Badge/status**: dot before word; success/warn/danger tints.
- **Stat (metric card)**: `.stat` column with `.stat-value` (24px Plex Mono, tabular figures, weight 500, ink) and `.stat-label` (12px ink-2). Container reuses `.panel`. Used for AI usage, subscription counts, dashboard metrics.

## 6. Motion
- 200ms ease transitions.
- Respect `prefers-reduced-motion`.

## 7. Layout
- Max width 1040px, centered.
- Single column mobile; two-column desktop.

## 8. Do / Don't
- Do: generous whitespace, one CTA per screen, clear empty states.
- Don't: clutter, multiple competing colors, dense tables, jargon.
## Materialization
The tokens live in `assets/app.css` (served from the binary — no CDN). Light is
the default; `[data-theme="dark"]` mirrors it. Change the tokens there, and the
whole console follows. The operator console layout: left rail + ledger-style
tables; setup and sign-in use the focused front-door shell.
