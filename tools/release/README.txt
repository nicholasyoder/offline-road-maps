OFFLINE ROAD MAPS  @VERSION@
================================================================================
Road maps, search and driving directions for North America that work with no
internet connection. Nothing in this folder ever connects to a network.


WHAT IS IN THIS FOLDER
  OfflineRoadMaps.exe   the program for Windows
  OfflineRoadMaps.sh    starts the program on Linux
  linux/                the Linux program, and an example menu entry
  maps/                 the maps (a map is a folder of files ending in .omp.000,
                        .omp.001 and so on; keep them together)
  licenses/             the licences of the program and of everything in it
  config.toml, trips/, pins/
                        your settings, saved trips and pushpins; the program
                        makes them here the first time you use them


START IT
  Windows   Double-click OfflineRoadMaps.exe. If Windows warns that the program
            is from an unknown publisher, choose "More info", then "Run anyway":
            it is not signed.
  Linux     Open a terminal in this folder and run
                sh OfflineRoadMaps.sh
            Run it this way, not linux/OfflineRoadMaps itself: the launcher also
            works from disks that do not allow running programs.
            It needs a graphics driver with Vulkan or OpenGL, and glibc 2.35 or
            newer (Ubuntu 22.04, Linux Mint 21, Debian 12 or later).

The map opens on its own. With several maps in maps/, it opens the one you used
last, or offers "Choose a map" (Ctrl+O).
Press F1 for the keys and the mouse controls. The menu button (three lines) has
Settings, About this map (where the data comes from) and printing.


THE BASICS
  Pan        drag the map, or use the arrow keys.
  Zoom       scroll, double-click, or + and -.
  Search     Ctrl+F or / : type a place, a street or an address. Pick a result.
  Route      the Route page: set the start and end by searching, or right-click
             the map. The directions are listed; print them or save a PDF.
  Trips      save a route with its stops on the Trips page; open it any time.
  Pushpins   right-click the map, "Add pushpin". They are kept in sets.
  Print      Menu > Print: a route's map and directions, to a printer or a PDF.


YOUR FILES
  Everything you save is in this folder: config.toml (settings), trips/ and
  pins/ (plain text files, one per trip or set), and PDFs. Copy the whole
  folder to another stick or computer and it all goes with it. If the folder
  cannot be written to (a write-protected stick), settings go to the computer's
  own settings folder and saved trips ask where to put the file.

  To add or change maps, put the map's folder inside maps/ or remove one. To
  start again from scratch, delete config.toml, trips/ and pins/.


THE STICK'S FILE SYSTEM
  Use exFAT. It holds the big map files, and Windows, Linux and macOS can read
  it. FAT32 also works (the maps are cut into pieces under 4 GB for it), but a
  USB 2 stick opens the map slowly on it, and on Linux the program is copied to
  ~/.cache/OfflineRoadMaps/ each time it starts.


LIMITS
  Cars only: routes are for driving, with turn restrictions and one-way streets,
  but without live traffic or road closures. No live position (no GPS). Labels
  are in Latin script. Address search is only as good as the data: addresses in
  rural areas and some regions are missing.


MAP DATA, LICENCES, NO WARRANTY
  Map data (c) OpenStreetMap contributors, available under the Open Database
  License (ODbL) from openstreetmap.org/copyright. House numbers also come from
  OpenAddresses, the US Census Bureau (TIGER) and Statistics Canada's National
  Address Register, each under its own licence; "About this map" in the program
  lists them. Time zone boundaries: timezone-boundary-builder.

  The program is free software under the MIT or Apache-2.0 licence (licenses/).
  The fonts, icons and libraries in it have their own, printed in
  licenses/THIRD-PARTY.txt.

  There is NO WARRANTY. Maps and routes can be wrong or out of date. Do not
  navigate by this alone: obey road signs and use your judgment.


PROBLEMS AND SOURCE
  https://github.com/nicholasyoder/offline-road-maps  (source code, how to build
  your own maps, and where to report a problem)
