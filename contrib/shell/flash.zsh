# Optional Flash-inspired zsh prompt. This file contains no user-specific text.
autoload -Uz colors vcs_info
colors

zstyle ':vcs_info:git:*' formats ' %F{242}git:%F{208}%b%f'
zstyle ':vcs_info:git:*' actionformats ' %F{242}git:%F{208}%b|%a%f'

flash_prompt_precmd() {
  vcs_info
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd flash_prompt_precmd
setopt prompt_subst

PROMPT='%F{208}%~%f${vcs_info_msg_0_}
%F{208}%n%f%F{242}@%f%F{250}%m%f %F{208}›%f '
