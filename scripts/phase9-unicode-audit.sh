#!/bin/sh

printf '\033]2;Flash Phase 9 Unicode audit\007'
printf 'PTY size: '
stty size
printf 'ASCII: Hello Flash\r\n'
printf 'Unicode: café naïve résumé\r\n'
printf 'Symbols: ✓ ✗ → ← ↑ ↓ ★ ♥ λ π ∞\r\n'
printf 'CJK: 日本語 中文 한국어\r\n'
printf 'Combining: é ä ñ\r\n'
printf 'Emoji: 😀 🚀 ❤️ 👍🏽\r\n'
printf '\033[38;2;255;80;80mred Unicode 日本語\033[0m '
printf '\033[38;5;39mblue Unicode 한국어\033[0m\r\n'
printf 'primary-before-alt\r\n'
printf '\033[?1049halternate-screen\033[?1049l'
printf 'primary-after-alt\r\n'
# Keep the PTY alive long enough for cold fontconfig/CJK fallback loading to
# complete, so the audit observes the incremental atlas update as well as the
# replacement-glyph first frame.
sleep 4
