;; Counts the words in a page's text.
;;
;; Deliberately trivial, and deliberately readable: the point of an example tool
;; is to show the shape of the contract, and something you can review by eye is
;; a better demonstration of a sandbox than something you cannot.
;;
;; Note what is absent — there are no imports. This module cannot read a file,
;; open a socket, look at the clock or learn what it is being run against. It is
;; handed bytes and it returns bytes.
(module
  (memory (export "memory") 1)
  (global $next (mut i32) (i32.const 1024))

  ;; The host calls this to make room for the input.
  (func $alloc (export "alloc") (param $len i32) (result i32)
    (local $at i32)
    (local $needed i32)
    (local.set $at (global.get $next))
    (local.set $needed (i32.add (local.get $at) (i32.add (local.get $len) (i32.const 64))))
    (if (i32.gt_u (local.get $needed) (i32.mul (memory.size) (i32.const 65536)))
      (then
        (drop (memory.grow
          (i32.add
            (i32.div_u (i32.sub (local.get $needed) (i32.mul (memory.size) (i32.const 65536)))
                       (i32.const 65536))
            (i32.const 1))))))
    (global.set $next (i32.add (global.get $next) (i32.add (local.get $len) (i32.const 8))))
    (local.get $at))

  ;; Returns the output pointer in the high half and its length in the low half.
  (func (export "run") (param $ptr i32) (param $len i32) (result i64)
    (local $i i32) (local $c i32) (local $inside i32) (local $words i32)
    (local $out i32) (local $digits i32) (local $left i32) (local $j i32) (local $k i32)

    ;; A word is a run of anything that is not whitespace.
    (block $counted
      (loop $step
        (br_if $counted (i32.ge_u (local.get $i) (local.get $len)))
        (local.set $c (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
        (if (i32.le_u (local.get $c) (i32.const 32))
          (then (local.set $inside (i32.const 0)))
          (else
            (if (i32.eqz (local.get $inside))
              (then
                (local.set $words (i32.add (local.get $words) (i32.const 1)))
                (local.set $inside (i32.const 1))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $step)))

    ;; Render it as decimal, least significant digit first, then reverse.
    (local.set $out (call $alloc (i32.const 24)))
    (local.set $left (local.get $words))
    (if (i32.eqz (local.get $left))
      (then
        (i32.store8 (local.get $out) (i32.const 48))
        (local.set $digits (i32.const 1)))
      (else
        (block $rendered
          (loop $digit
            (br_if $rendered (i32.eqz (local.get $left)))
            (i32.store8
              (i32.add (local.get $out) (local.get $digits))
              (i32.add (i32.const 48) (i32.rem_u (local.get $left) (i32.const 10))))
            (local.set $digits (i32.add (local.get $digits) (i32.const 1)))
            (local.set $left (i32.div_u (local.get $left) (i32.const 10)))
            (br $digit)))
        (local.set $j (i32.const 0))
        (local.set $k (i32.sub (local.get $digits) (i32.const 1)))
        (block $reversed
          (loop $swap
            (br_if $reversed (i32.ge_s (local.get $j) (local.get $k)))
            (local.set $c (i32.load8_u (i32.add (local.get $out) (local.get $j))))
            (i32.store8
              (i32.add (local.get $out) (local.get $j))
              (i32.load8_u (i32.add (local.get $out) (local.get $k))))
            (i32.store8 (i32.add (local.get $out) (local.get $k)) (local.get $c))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (local.set $k (i32.sub (local.get $k) (i32.const 1)))
            (br $swap)))))

    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32))
      (i64.extend_i32_u (local.get $digits)))))
