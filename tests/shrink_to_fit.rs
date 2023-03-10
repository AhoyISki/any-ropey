extern crate rand;
extern crate ropey;

use rand::{rngs::ThreadRng, Rng};
use ropey::{Measurable, Rope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lipsum {
    Lorem,
    Ipsum,
    Dolor(usize),
    Sit,
    Amet,
    Consectur(&'static str),
    Adipiscing(bool),
}

impl Measurable for Lipsum {
    fn width(&self) -> usize {
        match self {
            Lipsum::Lorem => 1,
            Lipsum::Ipsum => 2,
            Lipsum::Dolor(width) => *width,
            Lipsum::Sit => 0,
            Lipsum::Amet => 0,
            Lipsum::Consectur(text) => text.len(),
            Lipsum::Adipiscing(boolean) => *boolean as usize,
        }
    }
}
use self::Lipsum::*;

fn random_slice(rng: &mut ThreadRng, len: usize) -> Vec<Lipsum> {
    (0..rng.gen::<usize>() % 10)
        .into_iter()
        .map(|_| match rng.gen::<usize>() % 14 {
            0 | 7 => Lorem,
            1 | 8 => Ipsum,
            2 => Dolor(4),
            3 | 10 => Sit,
            4 | 11 => Amet,
            5 => Consectur("hello"),
            6 => Adipiscing(true),
            9 => Dolor(8),
            12 => Consectur("bye"),
            13 => Adipiscing(false),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
#[cfg_attr(miri, ignore)]
fn shrink_to_fit() {
    let mut rng = rand::thread_rng();
    let mut rope = Rope::new();

    // Do a bunch of random incoherent inserts
    for _ in 0..(1 << 12) {
        let len = rope.len().max(1);
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
        rope.insert_slice(rng.gen::<usize>() % len, random_slice(&mut rng, len).as_slice());
    }

    let rope2 = rope.clone();
    rope.shrink_to_fit();

    assert_eq!(rope, rope2);
    assert!(rope.capacity() < rope2.capacity());

    // Make sure the rope is sound
    rope.assert_integrity();
    rope.assert_invariants();

    rope2.assert_integrity();
    rope2.assert_invariants();
}
