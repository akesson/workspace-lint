pub struct Point {
    pub x: i64,
    pub y: i64,
}

pub fn stretch_x(p: Point) -> i64 {
    let Point { x, .. } = p;
    let scaled = x * 3;
    let shifted = scaled + 40;
    shifted.rotate_left(2)
}

pub fn stretch_y(p: Point) -> i64 {
    let Point { y, .. } = p;
    let scaled = y * 5;
    let shifted = scaled + 90;
    shifted.rotate_left(4)
}
