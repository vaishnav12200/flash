#!/bin/sh

printf '\033]2;Flash visual audit\007'
printf 'Flash visual system\r\n'
printf 'normal  \033[1mbold\033[0m  \033[2mdim\033[0m  \033[4munderline\033[0m  \033[7minverse\033[0m\r\n'
printf '\033[30mblack\033[0m \033[31mred\033[0m \033[32mgreen\033[0m \033[33myellow\033[0m '
printf '\033[34mblue\033[0m \033[35mmagenta\033[0m \033[36mcyan\033[0m \033[37mwhite\033[0m\r\n'
printf '\033[90mbright black\033[0m \033[91mbright red\033[0m \033[92mbright green\033[0m '
printf '\033[93mbright yellow\033[0m \033[94mbright blue\033[0m \033[95mbright magenta\033[0m '
printf '\033[96mbright cyan\033[0m \033[97mbright white\033[0m\r\n'
printf '\033[38;5;208m256 orange\033[0m  \033[38;2;80;180;255mtruecolor blue\033[0m\r\n'
printf 'Unicode: café naïve résumé  日本語 中文 한국어  ✓ → λ π ∞  😀 🚀\r\n'
printf 'Powerline/Nerd symbols:    \r\n'
printf '\r\nSelect across ANSI and Unicode rows to audit selection contrast.\r\n'
sleep 6
