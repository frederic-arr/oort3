use crate::bullet::{BulletAccessor, BulletAccessorMut, BulletData, BulletHandle};
use crate::debug;
pub use crate::debug::Line;
use crate::index_set::IndexSet;
use crate::scenario;
use crate::scenario::Scenario;
use crate::script;
use crate::script::{ShipController, TeamController};
use crate::ship::{ShipAccessor, ShipAccessorMut, ShipData, ShipHandle};
use crate::snapshot::*;
use crossbeam::channel::Sender;
use nalgebra::Vector2;
use rapier2d_f64::prelude::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

pub const WORLD_SIZE: f64 = 10000.0;
pub const PHYSICS_TICK_LENGTH: f64 = 1.0 / 60.0;

pub struct Simulation {
    scenario: Option<Box<dyn Scenario>>,
    pub ships: IndexSet<ShipHandle>,
    pub(crate) ship_data: HashMap<ShipHandle, ShipData>,
    team_controllers: HashMap<i32, Rc<RefCell<Box<dyn TeamController>>>>,
    pub(crate) ship_controllers: HashMap<ShipHandle, Box<dyn ShipController>>,
    pub bullets: IndexSet<BulletHandle>,
    pub(crate) bullet_data: HashMap<BulletHandle, BulletData>,
    pub(crate) bodies: RigidBodySet,
    pub(crate) impulse_joints: ImpulseJointSet,
    pub(crate) multibody_joints: MultibodyJointSet,
    pub(crate) colliders: ColliderSet,
    integration_parameters: IntegrationParameters,
    physics_pipeline: PhysicsPipeline,
    pub(crate) island_manager: IslandManager,
    broad_phase: BroadPhase,
    narrow_phase: NarrowPhase,
    ccd_solver: CCDSolver,
    event_collector: CollisionEventHandler,
    contact_recv: crossbeam::channel::Receiver<CollisionEvent>,
    pub(crate) events: SimEvents,
    tick: u32,
    pub cheats: bool,
    seed: u32,
}

impl Simulation {
    pub fn new(scenario_name: &str, seed: u32, mut code: &str) -> Box<Simulation> {
        if code.is_empty() {
            code = "fn tick(){}";
        }
        let (contact_send, contact_recv) = crossbeam::channel::unbounded();
        let mut sim = Box::new(Simulation {
            scenario: None,
            ships: IndexSet::new(),
            ship_data: HashMap::new(),
            team_controllers: HashMap::new(),
            ship_controllers: HashMap::new(),
            bullets: IndexSet::new(),
            bullet_data: HashMap::new(),
            bodies: RigidBodySet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            colliders: ColliderSet::new(),
            integration_parameters: IntegrationParameters {
                dt: PHYSICS_TICK_LENGTH,
                max_ccd_substeps: 2,
                ..Default::default()
            },
            physics_pipeline: PhysicsPipeline::new(),
            island_manager: IslandManager::new(),
            broad_phase: BroadPhase::new(),
            narrow_phase: NarrowPhase::new(),
            ccd_solver: CCDSolver::new(),
            event_collector: CollisionEventHandler::new(contact_send),
            contact_recv,
            events: SimEvents::new(),
            tick: 0,
            cheats: false,
            seed,
        });

        sim.upload_code(/*team=*/ 0, code);

        let mut scenario = scenario::load(scenario_name);
        scenario.init(&mut sim, seed);
        sim.scenario = Some(scenario);

        sim
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }

    pub fn time(&self) -> f64 {
        self.tick as f64 * PHYSICS_TICK_LENGTH
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }

    pub fn status(&self) -> scenario::Status {
        self.scenario.as_ref().unwrap().status(self)
    }

    pub fn ship(self: &Simulation, handle: ShipHandle) -> ShipAccessor {
        ShipAccessor {
            simulation: self,
            handle,
        }
    }

    pub fn ship_mut(self: &mut Simulation, handle: ShipHandle) -> ShipAccessorMut {
        ShipAccessorMut {
            simulation: self,
            handle,
        }
    }

    pub fn bullet(self: &Simulation, handle: BulletHandle) -> BulletAccessor {
        BulletAccessor {
            simulation: self,
            handle,
        }
    }

    pub fn bullet_mut(self: &mut Simulation, handle: BulletHandle) -> BulletAccessorMut {
        BulletAccessorMut {
            simulation: self,
            handle,
        }
    }

    #[allow(clippy::let_unit_value)]
    pub fn step(self: &mut Simulation) {
        self.events.clear();
        let gravity = vector![0.0, 0.0];
        let physics_hooks = ();

        self.physics_pipeline.step(
            &gravity,
            &self.integration_parameters,
            &mut self.island_manager,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.ccd_solver,
            &physics_hooks,
            &self.event_collector,
        );

        while let Ok(event) = self.contact_recv.try_recv() {
            if let CollisionEvent::Started(h1, h2, _flags) = event {
                let get_index = |h| self.colliders.get(h).and_then(|x| x.parent()).map(|x| x.0);
                let handle_hit = |sim: &mut Simulation, ship, bullet| {
                    if sim.bullet(bullet).data().team == sim.ship(ship).data().team {
                        sim.bullet_mut(bullet).destroy();
                        return;
                    }
                    let damage = sim.bullet(bullet).data().damage;
                    let ship_destroyed = {
                        let ship_data = sim.ship_data.get_mut(&ship).unwrap();
                        ship_data.health -= damage;
                        ship_data.health <= 0.0
                    };
                    if ship_destroyed {
                        sim.events
                            .ships_destroyed
                            .push(sim.ship(ship).body().position().translation.vector);
                        sim.ship_mut(ship).data_mut().destroyed = true;
                    }
                    sim.events
                        .hits
                        .push(sim.bullet(bullet).body().position().translation.vector);
                    sim.bullet_mut(bullet).destroy();
                };
                if let (Some(idx1), Some(idx2)) = (get_index(h1), get_index(h2)) {
                    if self.bullets.contains(BulletHandle(idx1)) {
                        if self.ships.contains(ShipHandle(idx2)) {
                            handle_hit(self, ShipHandle(idx2), BulletHandle(idx1));
                        } else {
                            self.bullet_mut(BulletHandle(idx1)).destroy();
                        }
                    } else if self.bullets.contains(BulletHandle(idx2)) {
                        if self.ships.contains(ShipHandle(idx1)) {
                            handle_hit(self, ShipHandle(idx1), BulletHandle(idx2));
                        } else {
                            self.bullet_mut(BulletHandle(idx2)).destroy();
                        }
                    }
                }
            }
        }

        let handle_snapshot: Vec<ShipHandle> = self.ships.iter().cloned().collect();
        for handle in handle_snapshot {
            if let Some(ship_controller) = self.ship_controllers.get_mut(&handle) {
                if let Err(e) = ship_controller.tick() {
                    self.events.errors.push(e);
                }
            }
            debug::emit_ship(self, handle);
            self.ship_mut(handle).tick();
        }

        let mut scenario = std::mem::take(&mut self.scenario);
        scenario.as_mut().unwrap().tick(self);
        self.scenario = scenario;

        self.tick += 1;
    }

    pub fn upload_code(&mut self, team: i32, code: &str) {
        match script::new_team_controller(code) {
            Ok(team_ctrl) => {
                self.team_controllers
                    .insert(team, Rc::new(RefCell::new(team_ctrl)));
            }
            Err(e) => {
                self.events.errors.push(e);
            }
        }
    }

    pub fn events(&self) -> &SimEvents {
        &self.events
    }

    pub fn emit_debug_lines(&mut self, ship: ShipHandle, lines: &[Line]) {
        self.events
            .debug_lines
            .entry(ship.into())
            .or_default()
            .extend(lines.iter().cloned());
    }

    pub fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let fixedpoint = |v: f64| (v * 1e9) as i64;
        let mut s = DefaultHasher::new();
        for handle in self.ships.iter() {
            let ship = self.ship(*handle);
            s.write_i64(fixedpoint(ship.position().x));
            s.write_i64(fixedpoint(ship.position().y));
            s.write_i64(fixedpoint(ship.heading()));
            s.write_i64(fixedpoint(ship.velocity().x));
            s.write_i64(fixedpoint(ship.velocity().y));
            s.write_i64(fixedpoint(ship.angular_velocity()));
            s.write_i64(fixedpoint(ship.data().health));
        }
        s.finish()
    }

    pub fn snapshot(&self, nonce: u32) -> Snapshot {
        let mut snapshot = Snapshot {
            nonce,
            time: self.time(),
            status: self.status(),
            ships: vec![],
            bullets: vec![],
            scenario_lines: self.scenario.as_ref().unwrap().lines(),
            debug_lines: self.events.debug_lines.clone(),
            hits: self.events.hits.clone(),
            ships_destroyed: self.events.ships_destroyed.clone(),
            errors: self.events.errors.clone(),
            cheats: self.cheats,
        };

        for &handle in self.ships.iter() {
            let ship = self.ship(handle);
            let id = handle.into();
            let position = ship.position().vector.into();
            let team = ship.data().team;
            let class = ship.data().class;
            let health = ship.data().health;
            snapshot.ships.push(ShipSnapshot {
                id,
                position,
                velocity: ship.velocity(),
                heading: ship.heading(),
                angular_velocity: ship.angular_velocity(),
                team,
                class,
                health,
            });
        }

        for &handle in self.bullets.iter() {
            let bullet = self.bullet(handle);
            snapshot.bullets.push(BulletSnapshot {
                position: bullet.body().position().translation.vector.into(),
                velocity: *bullet.body().linvel(),
            });
        }

        snapshot
    }

    pub fn get_team_controller(
        &mut self,
        team: i32,
    ) -> Option<Rc<RefCell<Box<dyn TeamController>>>> {
        self.team_controllers.get_mut(&team).map(|x| x.clone())
    }
}

pub struct CollisionEventHandler {
    collision_event_sender: Sender<CollisionEvent>,
}

impl CollisionEventHandler {
    pub fn new(collision_event_sender: Sender<CollisionEvent>) -> CollisionEventHandler {
        CollisionEventHandler {
            collision_event_sender,
        }
    }
}

impl EventHandler for CollisionEventHandler {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        event: CollisionEvent,
        _contact_pair: Option<&rapier2d_f64::geometry::ContactPair>,
    ) {
        let _ = self.collision_event_sender.send(event);
    }
}

pub struct SimEvents {
    pub errors: Vec<script::Error>,
    pub hits: Vec<Vector2<f64>>,
    pub ships_destroyed: Vec<Vector2<f64>>,
    pub debug_lines: BTreeMap<u64, Vec<Line>>,
}

impl SimEvents {
    pub fn new() -> Self {
        Self {
            errors: vec![],
            hits: vec![],
            ships_destroyed: vec![],
            debug_lines: BTreeMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.errors.clear();
        self.hits.clear();
        self.ships_destroyed.clear();
        self.debug_lines.clear();
    }
}

impl Default for SimEvents {
    fn default() -> Self {
        SimEvents::new()
    }
}