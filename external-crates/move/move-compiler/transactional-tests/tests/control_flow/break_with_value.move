//# publish
module 0x42::m {
    public fun t0() {
        let _x = loop { break 0 };
    }

    public fun t1(): u64 {
        loop { break 0 } 
    }

    public fun t2(cond: bool): bool {
        if (cond) {
            loop { break true }
        } else {
            loop { break false }
        }
    }

    public fun t3(cond: bool): bool {
        loop { 
            break if (cond) {
                loop { break true }
            } else {
                loop { break false }
            }
        } 
    }
 
    public fun t4(cond: bool): u64 {
        let x = 0;
        loop { 
            if (cond) {
                break x  
            } else {
                x = x + 1;
            }
        } 
    }

    public fun t5(): u64 {
        let x = 0;
        if (loop { 
            if (x > 10) {
                break x
            } else {
                x = x + 1;
            }
        } == 0) {
            x
        } else {
            0
        }
    } 

    public fun t6(): bool {
        loop {
            break loop {
                break true
            }
        }
    }

    struct R {f: u64}
    
    public fun t7(): u64 {
        let R { f } = loop {
            break R { f: 0 }
        };
        f
    }
}

//# run
script {
use 0x42::m;

fun main() {
    m::t0();
    assert!(m::t1() == 0, 1);
    assert!(m::t2(true) == true, 2);
    assert!(m::t3(true) == true, 3);
    assert!(m::t4(true) == 0, 4);
    assert!(m::t5() == 0, 5);
    assert!(m::t6() == true, 6);
    assert!(m::t7() == 0, 7);
}

}
