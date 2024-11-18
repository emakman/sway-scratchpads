# sway-scratchpads

A simple manager for HUDS in sway.

It stores a list of scratchpad commands and associated client ids. When you tell it to show a scratchpad, it makes sure there is a client with that id (by running the command if it needs to) and then shows that client. When you tell it to hide a scratchpad it hides the client with that id.

Runs as a daemon so it can pay attention when new clients are opened and send them to the scratchpad if they're configured as scratchpad windows.

## Installation
`make install`
