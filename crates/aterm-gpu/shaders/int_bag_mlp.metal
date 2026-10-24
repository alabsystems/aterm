// int32-exact/v1's bag-of-ids MLP forward pass (design §12.2), one thread per row: integer
// only, sign and magnitude, no signed division or shift. aterm_gpu::compute prepends the
// prelude that names every job and params header word, generated from int_bag_mlp's tables.

#include <metal_stdlib>
using namespace metal;

constant uint FC_EMBED [[function_constant(0)]];
constant uint FC_HIDDEN [[function_constant(1)]];
constant uint FC_CLASSES [[function_constant(2)]];
constant uint FC_MIN_IDS [[function_constant(3)]];
constant uint FC_RUN_SALT [[function_constant(4)]];
constant uint RUN_SALT = is_function_constant_defined(FC_RUN_SALT) ? FC_RUN_SALT : 0xffffffffu;

kernel void k_int_bag_mlp(device const uint* job [[buffer(0)]],
                          device const uchar* params [[buffer(1)]],
                          device const uint* offsets [[buffer(2)]],
                          device const ushort* ids [[buffer(3)]],
                          device uint* out [[buffer(4)]],
                          uint gid [[thread_position_in_grid]]) {
    if (gid >= job[JOB_ROWS]) {
        return;
    }
    // No thread gets here: ROWS <= 2^20 < 2^21 <= RUN_SALT. The branch keys the function cache.
    if (gid >= RUN_SALT) {
        out[0] = RUN_SALT;
        return;
    }
    device const uint* head = (device const uint*)params;
    device const char* emb = (device const char*)(params + head[PARAMS_EMB_OFF]);
    device const char* w1 = (device const char*)(params + head[PARAMS_W1_OFF]);
    device const char* w2 = (device const char*)(params + head[PARAMS_W2_OFF]);
    device const char* lut = (device const char*)(params + head[PARAMS_LUT_OFF]);
    device const int* b1 = (device const int*)(params + head[PARAMS_B1_OFF]);
    device const int* b2 = (device const int*)(params + head[PARAMS_B2_OFF]);
    device uint* row = out + gid * job[JOB_OUT_STRIDE];
    const uint start = offsets[gid];
    const uint n = offsets[gid + 1] - start;

    // Step 1: abstain.
    if (n < FC_MIN_IDS) {
        for (uint c = 0; c < FC_CLASSES; c++) {
            row[c] = 0;
        }
        row[FC_CLASSES] = FC_CLASSES;
        return;
    }

    // Step 2: sum the embeddings.
    int S[32];
    for (uint j = 0; j < FC_EMBED; j++) {
        S[j] = 0;
    }
    for (uint i = start; i < start + n; i++) {
        device const char* e = emb + uint(ids[i]) * FC_EMBED;
        for (uint j = 0; j < FC_EMBED; j++) {
            S[j] += int(e[j]);
        }
    }

    const uint mult = head[PARAMS_MULT];
    const uint qmax = head[PARAMS_QMAX];
    const uint g = head[PARAMS_G];
    const int t = as_type<int>(head[PARAMS_T]);
    int h[128];
    for (uint k = 0; k < FC_HIDDEN; k++) {
        // Step 3: the first layer.
        int a = 0;
        for (uint j = 0; j < FC_EMBED; j++) {
            a += S[j] * int(w1[j * FC_HIDDEN + k]);
        }
        // Step 4: the mean, by a truncating unsigned division of the magnitude.
        const uint mag_a = a < 0 ? 0u - uint(a) : uint(a);
        const uint q = min(mag_a / n, qmax);
        // Step 5: requantize, then the bias.
        const int mag = int((q * mult) >> g);
        const int p = (a < 0 ? -mag : mag) + b1[k];
        // Step 6: the activation table.
        h[k] = int(lut[clamp(p, -t, t) + t]);
    }

    // Steps 7 and 8: the output layer, and the lowest class with the largest logit.
    int best_l = 0;
    uint best = 0;
    for (uint c = 0; c < FC_CLASSES; c++) {
        int l = b2[c];
        for (uint k = 0; k < FC_HIDDEN; k++) {
            l += h[k] * int(w2[k * FC_CLASSES + c]);
        }
        row[c] = as_type<uint>(l);
        if (c == 0 || l > best_l) {
            best_l = l;
            best = c;
        }
    }
    row[FC_CLASSES] = best;
}
