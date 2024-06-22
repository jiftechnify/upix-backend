/// Wraps each value with a Box::pin and collect them into a Vec.
macro_rules! map_pin {
    ($($e:expr),* $(,)?) => {
        vec![$(Box::pin($e)),*]
    };
}
