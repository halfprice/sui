module 0x42::M {
    fun t(cond: bool) {
        let _x = loop { break 0 };
    }
}
