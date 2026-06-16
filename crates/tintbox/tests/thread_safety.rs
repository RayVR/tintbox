//! The consumer-threading model (the library does NOT thread internally; consumers
//! split the pixel buffer across cores and share one `Transform`) requires that a
//! `Transform` is `Send + Sync`. Assert it at compile time so it can never regress.
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn transform_is_send_and_sync() {
    assert_send_sync::<tintbox::transform::Transform>();
}
