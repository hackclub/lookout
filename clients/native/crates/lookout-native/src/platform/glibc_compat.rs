//! Keep the Linux binary loadable on glibc 2.34 (RHEL/AlmaLinux/Rocky 9),
//! though it is built on Ubuntu 24.04's glibc 2.39.
//!
//! The linker binds each libc call to the newest version of the symbol in
//! the glibc it links against. Almost everything we call has had one version
//! for decades, but glibc 2.35 gave `hypotf` a new one, so `f32::hypot`
//! alone would make the binary refuse to start on 2.34. Defining it here
//! keeps that call out of libm. packaging/linux/packages.sh fails the build
//! if anything else needs a glibc newer than 2.34 (weak references, which
//! Rust's std uses to probe for newer calls, are fine).

/// `hypotf`, computed in `f64`, where squaring an `f32` can't overflow and
/// the rounding back to `f32` makes the result as exact as libm's.
#[unsafe(no_mangle)]
pub extern "C" fn hypotf(x: f32, y: f32) -> f32 {
    // IEEE 754: an infinite side wins even over a NaN.
    if x.is_infinite() || y.is_infinite() {
        return f32::INFINITY;
    }
    let (x, y) = (f64::from(x), f64::from(y));
    (x * x + y * y).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::hypotf;

    #[test]
    fn matches_the_definition() {
        assert_eq!(hypotf(3.0, 4.0), 5.0);
        assert_eq!(hypotf(-3.0, 4.0), 5.0);
        assert_eq!(hypotf(0.0, 0.0), 0.0);
        assert_eq!(hypotf(f32::MAX, f32::MAX), f32::INFINITY);
        // Tiny sides don't underflow to zero either.
        assert!((hypotf(1e-30, 1e-30) / 1e-30 - std::f32::consts::SQRT_2).abs() < 1e-6);
        assert_eq!(hypotf(f32::INFINITY, f32::NAN), f32::INFINITY);
        assert!(hypotf(1.0, f32::NAN).is_nan());
    }
}
