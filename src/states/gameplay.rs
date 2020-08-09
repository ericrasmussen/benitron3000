/// This module contains our main gameplay state and game update method. It is
/// used by `main.rs` to build the application.
/// The main responsibilities are:
///   1) initialize the game world (assets, prefabs, entities)
///   2) setup the dispatcher so the systems here won't run in other states
///   3) act as the game's state manager (deciding when to switch states)
use amethyst::{
    assets::{Handle, PrefabLoader, ProgressCounter, RonFormat},
    core::math::{Translation3, UnitQuaternion, Vector3},
    core::{transform::Transform, ArcThreadPool},
    ecs::prelude::{Dispatcher, DispatcherBuilder},
    input::{is_close_requested, is_key_down, VirtualKeyCode},
    prelude::*,
    renderer::{Camera, SpriteRender, SpriteSheet},
    window::ScreenDimensions,
};

use derive_new::new;

use crate::entities::{
    enemy::{Enemy, EnemyPrefab},
    laser::Laser,
    player::{Player, PlayerPrefab},
};

use crate::{
    components::collider::Collider,
    resources::{
        handles,
        handles::GameplayHandles,
        level::{EnemyCount, EntityRecord, EntityType, LevelComplete, Levels},
    },
    states::paused::PausedState,
    systems,
};

use log::info;

#[derive(new)]
pub struct GameplayState<'a, 'b> {
    // keeps track of all the levels in our game
    pub levels: Levels,

    // collection of handles for creating sprites and prefabs
    #[new(default)]
    pub handles: Option<GameplayHandles>,

    // lets us build logic around whether or not assets are loaded
    #[new(default)]
    pub progress_counter: ProgressCounter,

    // dispatcher used to make sure this state's registered systems
    // won't run when other systems are active
    #[new(default)]
    pub dispatcher: Option<Dispatcher<'a, 'b>>,
}

impl<'a, 'b> SimpleState for GameplayState<'a, 'b> {
    // runs once each time the program initializes a new `GameplayState`
    fn on_start(&mut self, data: StateData<'_, GameData<'_, '_>>) {
        let world = data.world;

        // creates a dispatcher to collect systems specific to this state
        let mut dispatcher_builder = DispatcherBuilder::new();

        dispatcher_builder.add(systems::PlayerSystem, "player_system", &[]);
        dispatcher_builder.add(systems::LaserSystem, "laser_system", &[]);
        dispatcher_builder.add(systems::CollisionSystem, "collision_system", &[]);
        dispatcher_builder.add(systems::AttackedSystem, "attacked_system", &[]);
        dispatcher_builder.add(systems::EnemyTrackingSystem, "enemy_tracking_system", &[]);
        dispatcher_builder.add(systems::EnemyMoveSystem, "enemy_move_system", &[]);
        // TODO: replace this with some kind of level transition state
        dispatcher_builder.add(systems::CleanupSystem, "cleanup_system", &[]);

        // builds and sets up the dispatcher
        let mut dispatcher = dispatcher_builder
            .with_pool((*world.read_resource::<ArcThreadPool>()).clone())
            .build();
        dispatcher.setup(world);

        self.dispatcher = Some(dispatcher);

        // Get the screen dimensions so we can initialize the camera and
        // place our sprites correctly later. We'll clone this since we'll
        // pass the world mutably to the following functions.
        let dimensions = (*world.read_resource::<ScreenDimensions>()).clone();

        // Place the camera
        init_camera(world, &dimensions);

        // easier to load the prefab handles here and then pass them to
        let enemy_prefab_handle = world.exec(|loader: PrefabLoader<'_, EnemyPrefab>| {
            loader.load("prefabs/enemy.ron", RonFormat, &mut self.progress_counter)
        });

        let flying_enemy_prefab_handle = world.exec(|loader: PrefabLoader<'_, EnemyPrefab>| {
            loader.load("prefabs/flying_enemy.ron", RonFormat, &mut self.progress_counter)
        });

        let player_prefab_handle = world.exec(|loader: PrefabLoader<'_, PlayerPrefab>| {
            loader.load("prefabs/player.ron", RonFormat, &mut self.progress_counter)
        });

        // load the remaining sprite sheets and collect all the handles used by `level_init`
        let gameplay_handles = handles::get_game_handles(
            world,
            &mut self.progress_counter,
            enemy_prefab_handle,
            flying_enemy_prefab_handle,
            player_prefab_handle,
        );
        self.handles = Some(gameplay_handles);

        // render the background
        init_background(
            world,
            &dimensions,
            self.handles.clone().unwrap().background_sprite_handle,
        );

        // register our entities and resources before inserting them or
        // having them created as part of `init_level` in `update`
        world.register::<Player>();
        world.register::<Laser>();
        world.register::<Enemy>();
        world.register::<Collider>();
        world.register::<LevelComplete>();
        world.register::<EnemyCount>();

        let enemy_count = EnemyCount { count: 0 };
        world.insert(enemy_count);

        let level_complete = LevelComplete::default();
        info!("inserting new level complete struct: {:?}", level_complete);
        world.insert(level_complete);
    }

    // need to review https://docs.amethyst.rs/stable/amethyst/prelude/struct.World.html
    // for other options
    fn update(&mut self, data: &mut StateData<'_, GameData<'_, '_>>) -> SimpleTrans {
        if let Some(dispatcher) = self.dispatcher.as_mut() {
            // TODO: we could maybe push another state over `GameplayState` and push back
            // when the counter is complete, rather than checking every time here
            if self.progress_counter.is_complete() {
                dispatcher.dispatch(&data.world);
            }
        }

        // probably a better way to do this, but we get the count as read only first
        // to avoid borrowing data.world
        let enemy_count = (*data.world.fetch::<EnemyCount>()).clone();
        //info!("enemy count is: {:?}", enemy_count);

        // this is our victory condition that lets us know the player finished
        // the level
        if enemy_count.count == 0 {
            //info!("enemy count reached 0");
            data.world.write_resource::<LevelComplete>().success = true;
        }

        let level_complete = (*data.world.fetch::<LevelComplete>()).clone();

        // however, we're not ready for the next level until multiple conditions
        // are met, so here we defer to `level_complete` (systems will write to
        // this too)
        if level_complete.ready_for_next_level() {
            let next_level = self.levels.pop();

            let handles = self.handles.clone().expect("failure accessing GameplayHandles struct");

            if let Some(level_entities) = next_level {
                let new_count = init_level(data.world, level_entities, handles);

                {
                    let mut write_enemy_count = data.world.write_resource::<EnemyCount>();
                    write_enemy_count.count = new_count;
                    //info!("new enemy count is: {}", new_count);
                }

                {
                    let mut write_level_status = data.world.write_resource::<LevelComplete>();
                    write_level_status.start_over();
                    info!("current level complete resource says: {:?}", *write_level_status);
                }
            }

            Trans::None
        }
        // otherwise, nothing to see here folks!
        else {
            Trans::None
        }
    }

    // handles pausing (toggling the `p` key) and closing (window close or pressing escape)
    fn handle_event(&mut self, _data: StateData<'_, GameData<'_, '_>>, event: StateEvent) -> SimpleTrans {
        if let StateEvent::Window(event) = &event {
            // Check if the window should be closed
            if is_close_requested(&event) || is_key_down(&event, VirtualKeyCode::Escape) {
                return Trans::Quit;
            }

            if is_key_down(&event, VirtualKeyCode::P) {
                return Trans::Push(Box::new(PausedState));
            }
        }

        // no state changes required
        Trans::None
    }
}

fn init_camera(world: &mut World, dimensions: &ScreenDimensions) {
    // Center the camera in the middle of the screen, and let it cover
    // the entire screen
    let mut transform = Transform::default();
    transform.set_translation_xyz(dimensions.width() * 0.5, dimensions.height() * 0.5, 1.);

    // many amethyst examples show using dimensions here, but it turns out we want the
    // intended dimensions (say, based on sprite sizes) and not the computed dimensions
    // (which are affected by hidpi and other factors, and may not be what we intended)
    world
        .create_entity()
        .with(Camera::standard_2d(1920.0, 1080.0))
        .with(transform)
        .build();
}

// render the background, giving it a low z value so it renders under
// everything else
fn init_background(world: &mut World, dimensions: &ScreenDimensions, bg_sprite_sheet_handle: Handle<SpriteSheet>) {
    let rotation = UnitQuaternion::from_euler_angles(0.0, 0.0, 0.0);

    let scale = Vector3::new(1.0, 1.0, 1.0);
    let position = Translation3::new(dimensions.width() * 0.5, dimensions.height() * 0.5, -25.0);
    let transform = Transform::new(position, rotation, scale);

    let bg_render = SpriteRender {
        sprite_sheet: bg_sprite_sheet_handle,
        sprite_number: 0,
    };

    world.create_entity().with(bg_render).with(transform).build();
}

// this could return the number of enemies generated, and a system
// could reduce that number as they're defeated
fn init_level(world: &mut World, entity_recs: Vec<EntityRecord>, handles: GameplayHandles) -> i32 {
    let rotation = UnitQuaternion::from_euler_angles(0.0, 0.0, 0.0);
    let scale = Vector3::new(0.25, 0.25, 0.25);

    let player_render = SpriteRender {
        sprite_sheet: handles.player_sprites_handle,
        sprite_number: 0,
    };

    let blob_render = SpriteRender {
        sprite_sheet: handles.enemy_sprites_handle.clone(),
        sprite_number: 1,
    };

    let flying_render = SpriteRender {
        sprite_sheet: handles.enemy_sprites_handle,
        sprite_number: 2,
    };

    let mut count = 0;

    for rec in entity_recs {
        if let (EntityType::BlobEnemy, x, y) = rec {
            let position = Translation3::new(x, y, 0.0);
            let transform = Transform::new(position, rotation, scale);
            world
                .create_entity()
                .with(handles.enemy_prefab_handle.clone())
                .with(blob_render.clone())
                .with(transform)
                .build();

            count += 1;
        }

        if let (EntityType::FlyingEnemy, x, y) = rec {
            let position = Translation3::new(x, y, 0.0);
            let transform = Transform::new(position, rotation, scale);
            world
                .create_entity()
                .with(handles.flying_enemy_prefab_handle.clone())
                .with(flying_render.clone())
                .with(transform)
                .build();

            count += 1;
        }

        if let (EntityType::Player, x, y) = rec {
            let position = Translation3::new(x, y, 0.0);
            let transform = Transform::new(position, rotation, scale);
            world
                .create_entity()
                .with(handles.player_prefab_handle.clone())
                .with(player_render.clone())
                .with(transform)
                .build();
        }
    }

    // should probably return this in a more helpful struct,
    // but as long as we make level decisions based on enemy
    // counts it's fine
    count
}