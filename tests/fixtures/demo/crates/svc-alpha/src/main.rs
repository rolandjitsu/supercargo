fn main() {
    let value = either::Either::<u32, u32>::Left(common::answer());
    println!("alpha: {}", value.into_inner());
}
