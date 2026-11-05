OFFLINE ROAD MAPS  @VERSION@
================================================================================
Road maps, search and driving directions for North America that work with no
internet connection. Nothing in this folder ever connects to a network.

(This text is a first version; it is rewritten in phase 6 step 6.5.)

WHAT IS IN THIS FOLDER
  OfflineRoadMaps.exe   the program for Windows
  OfflineRoadMaps.sh    starts the program on Linux
  linux/                the Linux program, and an example menu entry
  maps/                 the maps (a map is a set of files ending in .omp.000,
                        .omp.001, ...); copy more maps here or remove some
  licenses/             licences of the program and of what it contains
  config.toml, trips/, pins/
                        your settings, saved trips and pushpins; the program
                        creates them here the first time you use them

START IT
  Windows:  double-click OfflineRoadMaps.exe.
  Linux:    open a terminal in this folder and run
                sh OfflineRoadMaps.sh
            (Do not run linux/OfflineRoadMaps yourself: the launcher also copes
            with disks that do not allow running programs.)

Press F1 in the program for the keys and the mouse controls.

THIS FOLDER IS THE PROGRAM'S WHOLE STATE
  You can copy the folder to another stick or computer. Your settings, trips and
  pins travel with it. The stick must not be write-protected, or settings are
  kept in the computer's own settings folder instead.

THE STICK'S FILE SYSTEM
  Use exFAT: it holds files over 4 GB and Linux can run the program from it.
  FAT32 also works, but the program is copied to ~/.cache/OfflineRoadMaps/
  at each start on Linux, and a USB 2 stick opens maps slowly.

LIMITS
  Cars only. No live position (no GPS). Labels in Latin script only.

MAP DATA
  (c) OpenStreetMap contributors, ODbL; addresses from the sources listed under
  "About this map" in the program. No warranty: do not navigate by this alone.
