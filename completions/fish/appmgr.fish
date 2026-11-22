# Fish completion for appmgr

complete -c appmgr -f

# Commands
complete -c appmgr -n "__fish_use_subcommand" -a install -d "Install application locally, from GitHub, or search by name"
complete -c appmgr -n "__fish_use_subcommand" -a search -d "Search GitHub repositories for applications"
complete -c appmgr -n "__fish_use_subcommand" -a link -d "Link installed application to a GitHub repository"
complete -c appmgr -n "__fish_use_subcommand" -a upgrade -d "Check and upgrade packages from GitHub Releases"
complete -c appmgr -n "__fish_use_subcommand" -a remove -d "Remove an installed application"
complete -c appmgr -n "__fish_use_subcommand" -a list -d "List installed applications"
complete -c appmgr -n "__fish_use_subcommand" -a info -d "Inspect package details or file"
complete -c appmgr -n "__fish_use_subcommand" -a doctor -d "Diagnose system health and dependencies"
complete -c appmgr -n "__fish_use_subcommand" -a run -d "Run an installed application"
complete -c appmgr -n "__fish_use_subcommand" -a env -d "Manage environment variables for an application"
complete -c appmgr -n "__fish_use_subcommand" -a args -d "Manage persistent launch arguments for an application"
complete -c appmgr -n "__fish_use_subcommand" -a pin -d "Freeze an application version to skip general upgrades"
complete -c appmgr -n "__fish_use_subcommand" -a unpin -d "Unfreeze an application version to allow updates"
complete -c appmgr -n "__fish_use_subcommand" -a rollback -d "Restore previous version of an application from cache"
complete -c appmgr -n "__fish_use_subcommand" -a clean -d "Clean rollback backups and temporary cache"
complete -c appmgr -n "__fish_use_subcommand" -a export -d "Export installed applications to a JSON manifest"
complete -c appmgr -n "__fish_use_subcommand" -a import -d "Install applications from a JSON manifest"
complete -c appmgr -n "__fish_use_subcommand" -a update-db -d "Update desktop database and caches"

# Flags
complete -c appmgr -l system -d "Force system installation"
complete -c appmgr -l user -d "Force user installation"
complete -c appmgr -s v -l version -d "Show version"
complete -c appmgr -s y -l yes -d "Assume yes to all prompts"

# Subcommand args
complete -c appmgr -n "__fish_seen_subcommand_from install" -s y -l yes -d "Assume yes"
complete -c appmgr -n "__fish_seen_subcommand_from install" -s f -l force -d "Force overwrite"
complete -c appmgr -n "__fish_seen_subcommand_from install" -l repo -d "Link to GitHub owner/repo"
complete -c appmgr -n "__fish_seen_subcommand_from install" -l prefer -a "appimage deb tar zip bin" -d "Preferred asset format"
complete -c appmgr -n "__fish_seen_subcommand_from install" -l pre -d "Allow pre-releases"
complete -c appmgr -n "__fish_seen_subcommand_from install" -r -F

complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -s c -l check -d "Check for updates without installing"
complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -s y -l yes -d "Assume yes"
complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -s f -l force -d "Force upgrade even if pinned"
complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -l prefer -a "appimage deb tar zip bin" -d "Preferred asset format"
complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -l pre -d "Include pre-releases in update check"
complete -c appmgr -n "__fish_seen_subcommand_from upgrade" -l json -d "Output machine-readable JSON"

complete -c appmgr -n "__fish_seen_subcommand_from list" -s t -l type -a "deb appimage tar zip bin" -d "Filter by format"
complete -c appmgr -n "__fish_seen_subcommand_from list" -l json -d "Output machine-readable JSON"
