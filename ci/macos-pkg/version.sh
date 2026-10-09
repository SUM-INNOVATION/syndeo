# Syndeo versions: what one looks like, and how two compare.
#
# Embedded into each of the package's root scripts when the package is built;
# no root script sources a file when it runs. The checks and helpers also use
# it directly.
#
# A version is three parts of digits, separated by dots, with no leading zeros
# and nothing else: 0.1.6, never 0.01.6, 0.1, 0.1.6-rc.1 or " 0.1.6". Two are
# compared part by part. A longer part is larger; parts of the same length are
# compared byte by byte, which for digits without leading zeros is their
# numeric order. No arithmetic is done on the numbers themselves, so no part
# is too large to compare.

# syndeo_version_valid V: whether V is exactly one well-formed version.
syndeo_version_valid() {
    case "$1" in
        '' | *[!0-9.]*) return 1 ;;
    esac
    printf '%s\n' "$1" | LC_ALL=C /usr/bin/grep -Eqx '(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
}

# syndeo_part_cmp A B: lt, eq or gt, for two parts of valid versions.
syndeo_part_cmp() {
    if [ "${#1}" -lt "${#2}" ]; then
        echo lt
    elif [ "${#1}" -gt "${#2}" ]; then
        echo gt
    elif [ "$1" = "$2" ]; then
        echo eq
    elif [ "$(printf '%s\n%s\n' "$1" "$2" | LC_ALL=C /usr/bin/sort | /usr/bin/head -n 1)" = "$1" ]; then
        echo lt
    else
        echo gt
    fi
}

# syndeo_version_cmp A B: lt, eq or gt, for two valid versions.
syndeo_version_cmp() {
    _sv_a=$1
    _sv_b=$2
    for _sv_i in 1 2 3; do
        _sv_r=$(syndeo_part_cmp "${_sv_a%%.*}" "${_sv_b%%.*}")
        if [ "$_sv_r" != eq ]; then
            echo "$_sv_r"
            return 0
        fi
        _sv_a=${_sv_a#*.}
        _sv_b=${_sv_b#*.}
    done
    echo eq
}
