// Verification-only (design §12.8 L1): echoes every job and params header word by the name
// the generated prelude gives it, in table order, so a misnamed index reads the wrong sentinel.

kernel void k_layout_echo(device const uint* job [[buffer(0)]],
                          device const uint* params [[buffer(1)]],
                          device uint* out [[buffer(2)]],
                          uint gid [[thread_position_in_grid]]) {
    if (gid != 0) {
        return;
    }
    out[0] = job[JOB_MAGIC];
    out[1] = job[JOB_LAYOUT_VERSION];
    out[2] = job[JOB_ROWS];
    out[3] = job[JOB_TOTAL_IDS];
    out[4] = job[JOB_OUT_STRIDE];
    out[5] = params[PARAMS_MAGIC];
    out[6] = params[PARAMS_LAYOUT_VERSION];
    out[7] = params[PARAMS_B];
    out[8] = params[PARAMS_E];
    out[9] = params[PARAMS_H];
    out[10] = params[PARAMS_C];
    out[11] = params[PARAMS_MIN_IDS];
    out[12] = params[PARAMS_MULT];
    out[13] = params[PARAMS_QMAX];
    out[14] = params[PARAMS_G];
    out[15] = params[PARAMS_T];
    out[16] = params[PARAMS_EMB_OFF];
    out[17] = params[PARAMS_EMB_LEN];
    out[18] = params[PARAMS_W1_OFF];
    out[19] = params[PARAMS_W1_LEN];
    out[20] = params[PARAMS_W2_OFF];
    out[21] = params[PARAMS_W2_LEN];
    out[22] = params[PARAMS_LUT_OFF];
    out[23] = params[PARAMS_LUT_LEN];
    out[24] = params[PARAMS_B1_OFF];
    out[25] = params[PARAMS_B1_LEN];
    out[26] = params[PARAMS_B2_OFF];
    out[27] = params[PARAMS_B2_LEN];
}
