# Ask a running Helix instance to open a file through its D-Bus interface
# (see helix-term/src/dbus/mod.rs). Requires `busctl` (part of systemd) and a
# Helix instance built with the default "dbus" feature.
#
# Usage:
#   just open path/to/file.rs
#   just open path/to/file.rs 42            # jump to line 42
#   just open path/to/file.rs 42 5          # ...and column 5
#   just open path/to/file.rs 0 0 false     # load in the background, don't switch to it
#   just open path/to/file.rs 0 0 true true # don't return until the buffer is closed
#
# By default this targets whichever instance claimed the shared
# "org.helix_editor.Helix" bus name (normally the first one started). Target
# a specific instance by passing its PID-suffixed name instead, e.g.:
#   just open path/to/file.rs 0 0 true false org.helix_editor.Helix.pid12345
open path line="0" column="0" focus="true" wait="false" instance="org.helix_editor.Helix":
    busctl --user --timeout=0 call {{instance}} /org/helix_editor/Helix org.helix_editor.Helix1 \
        OpenFile suubb "$(realpath "{{path}}")" {{line}} {{column}} {{focus}} {{wait}}

# Open a file and block until its buffer is closed in Helix, so this can be
# used as a blocking $EDITOR, e.g.:
#   EDITOR="just --justfile <path-to-this-justfile> edit" git commit
edit path line="0" column="0" instance="org.helix_editor.Helix":
    {{just_executable()}} --justfile '{{justfile()}}' open "{{path}}" {{line}} {{column}} true true {{instance}}

# List the absolute paths of a running instance's open, on-disk buffers.
open-files instance="org.helix_editor.Helix":
    busctl --user call {{instance}} /org/helix_editor/Helix org.helix_editor.Helix1 ListOpenFiles

list:
    busctl --user list | grep org.helix_editor.Helix || echo "No Helix instances found. "

# Regenerate helix-term/src/dbus/proxy.rs from helix-term/src/dbus/helix1.xml.
# Run this after editing the XML interface spec.
dbus-codegen:
    cargo xtask dbus-codegen
