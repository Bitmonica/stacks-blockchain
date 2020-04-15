#[macro_use]
extern crate criterion;
extern crate blockstack_lib;
extern crate rand;

use blockstack_lib::chainstate::burn::BlockHeaderHash;
use blockstack_lib::chainstate::stacks::index::{marf::MARF, storage::TrieFileStorage, MARFValue};

use criterion::Criterion;
use rand::prelude::*;
use std::fs;

fn benchmark_marf_usage(filename: &str, blocks: u32, writes_per_block: u32, reads_per_block: u32, batch: bool) {
    let f = TrieFileStorage::new(":memory:").unwrap();
    let mut block_header = BlockHeaderHash::from_bytes(&[0u8; 32]).unwrap();
    let mut marf = MARF::from_storage(f);
    marf.begin(&TrieFileStorage::block_sentinel(), &block_header).unwrap();
    
    let mut rng = rand::thread_rng();
    
    let mut values = vec![];
    
    for i in 0..blocks {
        if batch {
            let mut batch_keys = Vec::new();
            let mut batch_vals = Vec::new();
            for _k in 0..writes_per_block {
                let key: u64 = rng.gen();
                let key = key.to_string();
                let mut value = [0u8; 40];
                rng.fill_bytes(&mut value);
                batch_keys.push(key.clone());
                batch_vals.push(MARFValue(value.clone()));
                values.push((key, MARFValue(value)));
            }
            marf.insert_batch(&batch_keys, batch_vals).unwrap();
        } else {
            for _k in 0..writes_per_block {
                let key: u64 = rng.gen();
                let key = key.to_string();
                let mut value = [0u8; 40];
                rng.fill_bytes(&mut value);
                marf.insert(&key, MARFValue(value.clone())).unwrap();
                values.push((key, MARFValue(value)));
            }
        }
        
        for _k in 0..reads_per_block {
            let (key, value) = values.as_slice().choose(&mut rng).unwrap();
            assert_eq!(marf.get(&block_header, key).unwrap().unwrap(), *value);
        }
        
        let mut next_block_header = (i+1).to_le_bytes().to_vec();
        next_block_header.resize(32, 0);
        let next_block_header = BlockHeaderHash::from_bytes(next_block_header.as_slice()).unwrap();
            
        marf.commit().unwrap();
        marf.begin(&block_header, &next_block_header).unwrap();
        block_header = next_block_header;
        
    }
}

pub fn basic_usage_benchmark(c: &mut Criterion) {
    c.bench_function("marf_usage_1b_10kW_0kR", |b| b.iter(|| benchmark_marf_usage("/tmp/foo.bar.z", 1, 10000, 0, false)));
    c.bench_function("marf_usage_10b_1kW_2kR", |b| b.iter(|| benchmark_marf_usage("/tmp/foo.bar.z", 10, 1000, 2000, false)));
    c.bench_function("marf_usage_100b_5kW_20kR", |b| b.iter(|| benchmark_marf_usage("/tmp/foo.bar.z", 20, 5000, 20000, false)));
    c.bench_function("marf_usage_batches_10b_1kW_2kR", |b| b.iter(|| benchmark_marf_usage("/tmp/foo.bar.z", 10, 1000, 2000, true)));
}

pub fn scaling_read_ratio(_c: &mut Criterion) {
}

criterion_group!(benches, basic_usage_benchmark);
criterion_main!(benches);
