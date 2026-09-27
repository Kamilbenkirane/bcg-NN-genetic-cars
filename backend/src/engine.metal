#include <metal_stdlib>
using namespace metal;

// Philox4x32-10 follows Random123; see ../THIRD_PARTY_NOTICES.
constant uint RUNNING = 0;
constant uint CRASHED = 1;
constant uint FINISHED = 2;
constant uint TIMED_OUT = 3;
constant float PI = 3.14159265358979323846f;
constant float SENSOR_ANGLES[5] = {PI / 2, PI / 4, 0, -PI / 4, -PI / 2};

// Keep these scalar layouts synchronized with engine.rs and geometry.rs.
struct Params {
    uint population, max_steps, elite_count, mutation_count;
    uint generation, seed_low, seed_high, inspect_car;
    uint block_steps, record, node_count, center_count;
    float step_distance, sensor_range, max_heading_change, mutation_scale;
    float spawn_x, spawn_y, spawn_heading, spawn_distance;
    float lap_length, tolerance, finish_dx, finish_dy;
    uint first_step, target_laps, gate_index, reserved;
};

struct Wall {
    float2 a, b;
    uint kind, reserved0, reserved1, reserved2;
};

struct BvhNode {
    float4 bounds;
    uint first, count, escape, reserved;
};

struct CenterSegment {
    float2 a, delta;
    float length_sq, length, cumulative, reserved;
};

struct CarState {
    float x, y, heading, fitness;
    uint steps, status;
    float distance, previous_progress;
    uint completed_laps;
    float finish_fraction;
};

struct LapCrossing { uint step; float fraction; };

bool precedes(CarState a, uint ai, CarState b, uint bi) {
    bool af = a.status == FINISHED, bf = b.status == FINISHED;
    if (af != bf) return af;
    if (af) {
        uint at = a.steps + uint(a.finish_fraction == 1.0f);
        uint bt = b.steps + uint(b.finish_fraction == 1.0f);
        float ap = a.finish_fraction == 1.0f ? 0.0f : a.finish_fraction;
        float bp = b.finish_fraction == 1.0f ? 0.0f : b.finish_fraction;
        if (at != bt) return at < bt;
        if (ap != bp) return ap < bp;
    } else if (a.fitness != b.fitness) return a.fitness > b.fitness;
    return ai < bi;
}

struct Trace {
    uint step;
    float x, y, heading;
    float sensors[5];
    float steering;
};

uint4 philox(uint4 counter, uint2 key) {
    for (uint round = 0; round < 10; ++round) {
        uint lo0 = 0xD2511F53u * counter.x;
        uint hi0 = mulhi(0xD2511F53u, counter.x);
        uint lo1 = 0xCD9E8D57u * counter.z;
        uint hi1 = mulhi(0xCD9E8D57u, counter.z);
        counter = uint4(hi1 ^ counter.y ^ key.x, lo1,
                        hi0 ^ counter.w ^ key.y, lo0);
        key += uint2(0x9E3779B9u, 0xBB67AE85u);
    }
    return counter;
}

uint random_word(constant Params& p, uint stage, uint car, uint draw) {
    return philox(uint4(stage, p.generation, car, draw),
                  uint2(p.seed_low, p.seed_high)).x;
}

float uniform_open(uint word) {
    // Exactly representable values strictly between zero and one in f32.
    return (float(word >> 9) + 0.5f) * (1.0f / 8388608.0f);
}

float normal_weight(constant Params& p, uint stage, uint car, uint parameter) {
    float u = uniform_open(random_word(p, stage, car, parameter * 2));
    float v = uniform_open(random_word(p, stage, car, parameter * 2 + 1));
    return sqrt(-2.0f * log(u)) * cos(2.0f * PI * v);
}

bool is_bias(uint parameter) {
    return (parameter >= 15 && parameter < 18) || parameter == 21;
}

kernel void initialize_population(constant Params& p [[buffer(0)]],
                                  device float* genomes [[buffer(1)]],
                                  uint car [[thread_position_in_grid]]) {
    if (car >= p.population) return;
    for (uint parameter = 0; parameter < 22; ++parameter) {
        genomes[parameter * p.population + car] =
            is_bias(parameter) ? 0.0f : normal_weight(p, 0, car, parameter);
    }
}

kernel void reset_states(constant Params& p [[buffer(0)]],
                         device CarState* states [[buffer(1)]],
                         uint car [[thread_position_in_grid]]) {
    if (car >= p.population) return;
    states[car] = {p.spawn_x, p.spawn_y, p.spawn_heading, 0.0f,
                   0, RUNNING, 0.0f, p.spawn_distance, 0, 0.0f};
}

float cross2(float2 a, float2 b) {
    return a.x * b.y - a.y * b.x;
}

bool intersects_box(float2 origin, float2 delta, float4 bounds,
                    float tolerance, float nearest) {
    float low = 0.0f;
    float high = nearest;
    for (uint axis = 0; axis < 2; ++axis) {
        float minimum = bounds[axis] - tolerance;
        float maximum = bounds[axis + 2] + tolerance;
        if (delta[axis] == 0.0f) {
            if (origin[axis] < minimum || origin[axis] > maximum) return false;
        } else {
            float a = (minimum - origin[axis]) / delta[axis];
            float b = (maximum - origin[axis]) / delta[axis];
            low = max(low, min(a, b));
            high = min(high, max(a, b));
            if (low > high) return false;
        }
    }
    return true;
}

bool segment_contact(float2 origin, float2 delta, Wall wall, float tolerance,
                     thread float& t, thread float& u) {
    float2 edge = wall.b - wall.a;
    float2 relative = wall.a - origin;
    float ray_length = length(delta);
    float edge_length = length(edge);
    float denominator = cross2(delta, edge);
    float ray_slack = tolerance / ray_length;
    float edge_slack = tolerance / edge_length;
    if (abs(denominator) > 8.0f * FLT_EPSILON * ray_length * edge_length) {
        float ray_t = cross2(relative, edge) / denominator;
        float edge_t = cross2(relative, delta) / denominator;
        if (ray_t < -ray_slack || ray_t > 1.0f + ray_slack ||
            edge_t < -edge_slack || edge_t > 1.0f + edge_slack) return false;
        t = clamp(ray_t, 0.0f, 1.0f);
        u = clamp(edge_t, 0.0f, 1.0f);
        return true;
    }
    if (abs(cross2(relative, delta)) > tolerance * ray_length) return false;
    float inverse_length_sq = 1.0f / dot(delta, delta);
    float a = dot(relative, delta) * inverse_length_sq;
    float b = dot(wall.b - origin, delta) * inverse_length_sq;
    if (max(a, b) < -ray_slack || min(a, b) > 1.0f + ray_slack) return false;
    t = clamp(min(a, b), 0.0f, 1.0f);
    u = clamp(dot(origin + t * delta - wall.a, edge) / dot(edge, edge), 0.0f, 1.0f);
    return true;
}

struct Contact {
    float t;
    bool hit;
};

Contact nearest_contact(float2 origin, float2 delta, constant Params& p,
                        device const Wall* walls, device const BvhNode* nodes) {
    Contact nearest = {1.0f, false};
    float ray_length = length(delta);
    if (ray_length == 0.0f) return nearest;
    float ray_slack = p.tolerance / ray_length;
    uint node_index = 0;
    while (node_index < p.node_count) {
        BvhNode node = nodes[node_index];
        if (!intersects_box(origin, delta, node.bounds, p.tolerance,
                            min(1.0f, nearest.t + ray_slack))) {
            node_index = node.escape;
            continue;
        }
        if (node.count == 0) {
            ++node_index;
            continue;
        }
        for (uint index = node.first; index < node.first + node.count; ++index) {
            Wall wall = walls[index];
            if (wall.kind == 1) continue;
            float t, u;
            if (!segment_contact(origin, delta, wall, p.tolerance, t, u)) continue;
            if (!nearest.hit || t < nearest.t) nearest = {t, true};
        }
        node_index = node.escape;
    }
    return nearest;
}

float progress(float2 position, constant Params& p,
               device const CenterSegment* centerline) {
    float nearest_squared = INFINITY;
    float distance = 0.0f;
    for (uint i = 0; i < p.center_count; ++i) {
        CenterSegment segment = centerline[i];
        float t = clamp(dot(position - segment.a, segment.delta) / segment.length_sq,
                        0.0f, 1.0f);
        float2 offset = position - (segment.a + t * segment.delta);
        float squared = dot(offset, offset);
        if (squared < nearest_squared) {
            nearest_squared = squared;
            distance = segment.cumulative + t * segment.length;
        }
    }
    return distance;
}

float observe(float2 position, float heading, thread const float* weights,
              constant Params& p, device const Wall* walls,
              device const BvhNode* nodes, thread float* sensors, thread float* hidden) {
    for (uint i = 0; i < 5; ++i) {
        float angle = heading + SENSOR_ANGLES[i];
        float2 delta = p.sensor_range * float2(cos(angle), sin(angle));
        Contact hit = nearest_contact(position, delta, p, walls, nodes);
        sensors[i] = hit.t * p.sensor_range;
    }
    for (uint j = 0; j < 3; ++j) {
        float sum = weights[15 + j];
        for (uint i = 0; i < 5; ++i) sum += sensors[i] / p.sensor_range * weights[i * 3 + j];
        hidden[j] = tanh(sum);
    }
    float output = weights[21];
    for (uint j = 0; j < 3; ++j) output += hidden[j] * weights[18 + j];
    return tanh(output);
}

kernel void simulate_block(constant Params& p [[buffer(0)]],
                           device const float* genomes [[buffer(1)]],
                           device CarState* states [[buffer(2)]],
                           device const Wall* walls [[buffer(3)]],
                           device const BvhNode* nodes [[buffer(4)]],
                           device const CenterSegment* centerline [[buffer(5)]],
                           device float* poses [[buffer(6)]],
                           device Trace* traces [[buffer(7)]],
                           device atomic_uint* control [[buffer(8)]],
                           device LapCrossing* lap_ends [[buffer(9)]],
                           uint car [[thread_position_in_grid]]) {
    if (car >= p.population) return;
    CarState state = states[car];
    float weights[22];
    for (uint parameter = 0; parameter < 22; ++parameter)
        weights[parameter] = genomes[parameter * p.population + car];
    if (p.block_steps == 0 && car == p.inspect_car) {
        Trace trace;
        float hidden[3];
        trace.step = p.first_step;
        trace.x = state.x;
        trace.y = state.y;
        trace.heading = state.heading;
        trace.steering = observe(float2(state.x, state.y), state.heading, weights,
                                 p, walls, nodes, trace.sensors, hidden);
        traces[0] = trace;
    }
    for (uint frame = 0; frame < p.block_steps; ++frame) {
        if (state.status == RUNNING) {
            float sensors[5], hidden[3];
            float steering = observe(float2(state.x, state.y), state.heading, weights,
                                     p, walls, nodes, sensors, hidden);
            float heading = state.heading + clamp(steering, -1.0f, 1.0f) * p.max_heading_change;
            heading -= floor((heading + PI) / (2.0f * PI)) * (2.0f * PI);
            float2 delta = p.step_distance * float2(cos(heading), sin(heading));
            float2 position = float2(state.x, state.y);
            Contact hit = nearest_contact(position, delta, p, walls, nodes);
            // The timing line never blocks intermediate laps or sensors.
            if (state.distance > (float(state.completed_laps) + 0.75f) * p.lap_length &&
                dot(delta, float2(p.finish_dx, p.finish_dy)) > 0.0f) {
                Wall gate = walls[p.gate_index];
                float t, u;
                float edge_slack = p.tolerance / length(gate.b - gate.a);
                if (segment_contact(position, delta, gate, p.tolerance, t, u) &&
                    u > edge_slack && u < 1.0f - edge_slack &&
                    (!hit.hit || t < hit.t - p.tolerance / p.step_distance)) {
                    lap_ends[car * p.target_laps + state.completed_laps] = {state.steps, t};
                    ++state.completed_laps;
                    if (state.completed_laps == p.target_laps) {
                        hit.t = t;
                        state.finish_fraction = t;
                    }
                }
            }
            position += hit.t * delta;
            float along = progress(position, p, centerline);
            float traveled = along - state.previous_progress;
            // Unwrap signed progress at the seam: reversing cannot earn a lap.
            if (traveled > p.lap_length * 0.5f) traveled -= p.lap_length;
            if (traveled < -p.lap_length * 0.5f) traveled += p.lap_length;
            state.distance += traveled;
            state.previous_progress = along;
            state.x = position.x;
            state.y = position.y;
            state.heading = heading;
            ++state.steps;
            if (state.completed_laps == p.target_laps) {
                state.status = FINISHED;
            } else if (hit.hit) {
                state.status = CRASHED;
            } else if (state.steps >= p.max_steps) {
                state.status = TIMED_OUT;
            }
            float goal = p.lap_length * float(p.target_laps);
            state.fitness = state.status == FINISHED ? goal : clamp(state.distance, 0.0f, goal);
        }
        if (p.record != 0) {
            uint offset = (frame * p.population + car) * 3;
            poses[offset] = state.x;
            poses[offset + 1] = state.y;
            poses[offset + 2] = state.heading;
        }
        if (car == p.inspect_car) {
            Trace trace;
            float hidden[3];
            trace.step = p.first_step + frame;
            trace.x = state.x;
            trace.y = state.y;
            trace.heading = state.heading;
            trace.steering = observe(float2(state.x, state.y), state.heading, weights,
                                     p, walls, nodes, trace.sensors, hidden);
            traces[frame] = trace;
        }
    }
    states[car] = state;
    if (state.status == RUNNING)
        atomic_fetch_add_explicit(control, 1u, memory_order_relaxed);
    atomic_fetch_max_explicit(control + 1, state.steps, memory_order_relaxed);
}

// ponytail: O(N²) ranking is capped at 2,000 cars; use GPU radix sort if that cap grows.
kernel void rank_population(constant Params& p [[buffer(0)]],
                            device const CarState* states [[buffer(1)]],
                            device uint* ranked [[buffer(2)]],
                            uint car [[thread_position_in_grid]]) {
    if (car >= p.population) return;
    CarState self = states[car];
    uint rank = 0;
    for (uint other = 0; other < p.population; ++other) {
        CarState candidate = states[other];
        rank += precedes(candidate, other, self, car) ? 1 : 0;
    }
    ranked[rank] = car;
}

kernel void breed_population(constant Params& p [[buffer(0)]],
                             device const float* genomes [[buffer(1)]],
                             device const uint* ranked [[buffer(2)]],
                             device float* next [[buffer(3)]],
                             uint child [[thread_position_in_grid]]) {
    if (child >= p.population) return;
    uint copies = p.population / p.elite_count;
    float weights[22];
    if (child >= copies * p.elite_count) {
        for (uint parameter = 0; parameter < 22; ++parameter)
            weights[parameter] = is_bias(parameter) ? 0.0f : normal_weight(p, 2, child, parameter);
    } else {
        uint parent = ranked[child / copies];
        for (uint parameter = 0; parameter < 22; ++parameter)
            weights[parameter] = genomes[parameter * p.population + parent];
        if (child % copies != 0) {
            uint draw = 0;
            for (uint mutation = 0; mutation < p.mutation_count; ++mutation) {
                uint word;
                // Reject the small uneven tail so every scalar is equally likely.
                do { word = random_word(p, 1, child, draw++); } while (word < 4u);
                uint parameter = word % 22u;
                float uniform = uniform_open(random_word(p, 1, child, draw++));
                weights[parameter] += (2.0f * uniform - 1.0f) * p.mutation_scale;
            }
        }
    }
    for (uint parameter = 0; parameter < 22; ++parameter)
        next[parameter * p.population + child] = weights[parameter];
}

struct Summary {
    float best_fitness, mean_fitness;
    uint finished, crashed, timed_out, steps, car_steps, best_car_id;
};

kernel void summarize(constant Params& p [[buffer(0)]],
                      device const CarState* states [[buffer(1)]],
                      device Summary* output [[buffer(2)]],
                      uint id [[thread_position_in_grid]]) {
    if (id != 0) return;
    Summary summary = {0.0f, 0.0f, 0, 0, 0, 0, 0, 0};
    for (uint car = 0; car < p.population; ++car) {
        CarState state = states[car];
        if (precedes(state, car, states[summary.best_car_id], summary.best_car_id)) summary.best_car_id = car;
        summary.best_fitness = max(summary.best_fitness, state.fitness);
        summary.mean_fitness += state.fitness;
        summary.finished += state.status == FINISHED ? 1 : 0;
        summary.crashed += state.status == CRASHED ? 1 : 0;
        summary.timed_out += state.status == TIMED_OUT ? 1 : 0;
        summary.steps = max(summary.steps, state.steps);
        summary.car_steps += state.steps;
    }
    summary.mean_fitness /= float(p.population);
    *output = summary;
}
