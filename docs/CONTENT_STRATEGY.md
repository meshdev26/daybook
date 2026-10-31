# Content Strategy — Hub-and-Spoke Channel Plan

## Goal
Publish technical tutorials (WireGuard, Reality, etc.) across topic channels with a
single index ("Tech Tips") that links to them, without duplicate posts.

## Channels
| Channel (handle or ID) | Role | Audience |
|---|---|---|
| Tech Tips (`-1003876818154`) | Hub / index | Everyone — short summaries + links |
| (per-topic channels) | Spoke / detail | People interested in that topic |

## Roadmap (topics in order)
| # | Topic | Target channel | Status | Message link |
|---|---|---|---|---|
| 1 | WireGuard setup (server + client) | WireGuard channel | planned | — |
| 2 | VLESS + Reality setup | Reality channel | planned | — |
| 3 | (next topic) | … | planned | — |

Status values: `planned` → `drafted` → `posted` → `indexed` (linked from Tech Tips).

## Deduplication rules (prevent duplicate posts)
1. **Single source of truth**: this file's Roadmap table. Before posting, check the
   row for that topic+channel. If status is `posted` or `indexed`, do NOT repost.
2. **One owner per post**: only one AI session / operator posts a given topic.
   If another session is asked, it must read this file first and refuse if already posted.
3. **Record after posting**: update the row with `posted` status and the message link
   (`https://t.me/c/<channel_id>/<message_id>` or the public link) immediately.
4. **Index last**: Tech Tips links are added only after the spoke post is recorded.
5. **No re-posting on edit**: if a tutorial needs updating, edit the original
   message (Telegram: edit post) instead of posting a new one; update the link column.

## Per-post checklist
- [ ] Topic not already posted (check Roadmap table)
- [ ] Content uses placeholder values (`<YOUR_SERVER_IP>`, `<YOUR_KEY>`), no real secrets
- [ ] Posted to the correct spoke channel
- [ ] Row updated: status=`posted`, message link filled
- [ ] Tech Tips index updated with a short summary + link (status=`indexed`)

## AI posting workflow
When asked to post:
1. Read this file first.
2. If the topic+channel row is already `posted`/`indexed`, report "already posted" and stop.
3. Otherwise draft, post, then update this file's Roadmap table (status + link).
4. Never post the same content twice; prefer editing the original.

## Notes
- The console already tracks posts in the `posts` table (chat_id, message_id).
  Cross-reference with this file to avoid duplicates across AI sessions.
- Keep technical content accurate and defensive (setup guides, not attack tools).
EOF
echo "created"