# cs128H-final-project
- Group name: Hold-It-IN Inc. / The Soft Brown MATTER research institution
- Group member name: Yichen Cai, Yuhao Lu, Yunqi Lou

# Project name: Brownshock
# Project Introduction:

  Our project is a stealth-objective game tentatively titled "Stealth Poop." The player controls a character who desperately needs to relieve themselves in designated spots across a multi-floor building. The catch: they must remain entirely undetected by wandering NPCs. The act of pooping itself is a complex QTE (Quick Time Event) minigame featuring a progress bar and a "perfect hit" zone. It relies on mouse-clicking speed—faster clicks yield better progress, while poor timing slows it down, further complicated by a base RNG probability of failure (no progress gained). We chose this project because it is incredibly humorous and entertaining, while still providing a solid technical challenge in implementing stealth mechanics (field of view), state machines, and complex input-based probability math.

# Project Roadmap & Checkpoints:

- Checkpoint 1: Complete the basic grid layout for the rooms/floors, implement player movement, and build the core mathematical logic for the QTE minigame (calculating click speed vs. the base failure probability).

- Checkpoint 2: Introduce patrolling NPCs with line-of-sight (vision cone) detection, and implement the fail-state mechanics (getting caught and losing the game).

- Checkpoint 3: Finalize the UI rendering (visualizing the progress bar and the hit zone), add multi-floor transition mechanics, and balance the QTE difficulty.

# Possible Challenges:

- Line of Sight Algorithms: Implementing a fair and functional vision cone system for the NPCs so that the stealth mechanics feel accurate and don't unfairly punish the player through walls.

- Balancing the QTE Mechanics: Finding the right mathematical balance between the required mouse click speed and the base failure probability, ensuring the mechanic feels tense and frantic without being impossibly frustrating.
