use sapvi::{Decodable, ZKSupervisor};
use std::fs::File;
use std::time::Instant;

use bls12_381::Scalar;
use ff::{Field, PrimeField};
use group::{Curve, Group, GroupEncoding};
use rand::rngs::OsRng;

type Result<T> = std::result::Result<T, failure::Error>;

fn main() -> Result<()> {
    let start = Instant::now();
    let file = File::open("mint.zcd")?;
    let mut visor = ZKSupervisor::decode(file)?;
    println!("{}", visor.name);
    //ZKSupervisor::load_contract(bytes);
    println!("Finished: [{:?}]", start.elapsed());

    println!("Stats:");
    println!("    Constants: {}", visor.vm.constants.len());
    println!("    Alloc: {}", visor.vm.alloc.len());
    println!("    Operations: {}", visor.vm.ops.len());
    println!("    Constraint Instructions: {}", visor.vm.constraints.len());

    visor.vm.setup();

    let params = vec![
        (
            0,
            Scalar::from_raw([
                0xb981_9dc8_2d90_607e,
                0xa361_ee3f_d48f_df77,
                0x52a3_5a8c_1908_dd87,
                0x15a3_6d1f_0f39_0d88,
            ]),
        ),
        (
            1,
            Scalar::from_raw([
                0x7b0d_c53c_4ebf_1891,
                0x1f3a_beeb_98fa_d3e8,
                0xf789_1142_c001_d925,
                0x015d_8c7f_5b43_fe33,
            ]),
        ),
        (
            2,
            Scalar::from_raw([
                0xb981_9dc8_2d90_607e,
                0xa361_ee3f_d48f_df77,
                0x52a3_5a8c_1908_dd87,
                0x15a3_6d1f_0f39_0d88,
            ]),
        ),
        (
            3,
            Scalar::from_raw([
                0x7b0d_c53c_4ebf_1891,
                0x1f3a_beeb_98fa_d3e8,
                0xf789_1142_c001_d925,
                0x015d_8c7f_5b43_fe33,
            ]),
        ),
    ];
    visor.vm.initialize(&params);

    let proof = visor.vm.prove();

    let public = visor.vm.public();

    assert_eq!(public.len(), 2);
    // 0x66ced46f14e5616d12b993f60a6e66558d6b6afe4c321ed212e0b9cfbd81061a
    // 0x4731570fdd57cf280eadc8946fa00df81112502e44e497e794ab9a221f1bcca
    println!("u = {:?}", public[0]);
    println!("v = {:?}", public[1]);

    assert!(visor.vm.verify(&proof, &public));

    Ok(())
}
