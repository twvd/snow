# disk.sh: shell functions shared by the testers' scripts, to prepare disk images and get files
# out of them. Source it: . "$MAC_TESTS/common/disk.sh"
#
# hfsutils keeps a single current volume per user (in ~/.hcwd), so two scripts using it at the
# same time would step on each other: every function below holds a lock while it uses hfsutils.
# Never use them on a precious image: hfsutils rewrites the volume.

HFS_LOCK=${HFS_LOCK:-$HOME/.hcwd.lock}

# fresh_copy <source> <destination>: a copy of a base image, shared on disk where the file
# system allows it
fresh_copy() {
    cp --reflink=auto "$1" "$2"
}

# blank_disk <path> <size in MB>: a disk image full of zeroes
blank_disk() {
    rm -f "$1"
    truncate -s "$2M" "$1"
}

# install_app <image> <app.bin> <name>: copies a MacBinary application into the Startup Items
# folder of the System Folder, so that the Finder starts it at boot. Before System 7 there is
# no such folder: the application goes to the root of the volume, to be started by hand.
install_app() {
    (
        flock 9
        hmount "$1" >/dev/null
        if hcopy -m "$2" ":System Folder:Startup Items:$3" 2>/dev/null; then
            humount >/dev/null
        else
            hcopy -m "$2" ":$3"
            humount >/dev/null
            echo "no Startup Items folder in $1: start :$3 by hand" >&2
        fi
    ) 9>"$HFS_LOCK"
}

# put_empty_file <image> <path>: creates an empty file, e.g. a flag a tester looks for
put_empty_file() {
    (
        flock 9
        empty=$(mktemp)
        hmount "$1" >/dev/null
        hcopy -r "$empty" "$2"
        humount >/dev/null
        rm -f "$empty"
    ) 9>"$HFS_LOCK"
}

# get_file <image> <path> <output> [text|raw]: copies a file out of the image, its line ends
# converted for a text file (the default). Returns false if the file isn't there.
get_file() {
    mode=-t
    [ "${4:-text}" = raw ] && mode=-r
    (
        flock 9
        hmount "$1" >/dev/null
        status=0
        hcopy "$mode" "$2" "$3" 2>/dev/null || status=1
        humount >/dev/null
        exit $status
    ) 9>"$HFS_LOCK"
}
