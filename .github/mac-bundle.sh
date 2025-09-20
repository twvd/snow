# Taken from Panda3DS/pcsx-redux create-app-bundle.sh
# Used for Snow under MIT license with permission from Peach, Quist

# For Plist buddy
PATH="$PATH:/usr/libexec"

# Construct the app iconset.
#mkdir snow.iconset
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 72 -resize 16x16 snow.iconset/icon_16x16.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 144 -resize 32x32 snow.iconset/icon_16x16@2x.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 72 -resize 32x32 snow.iconset/icon_32x32.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 144 -resize 64x64 snow.iconset/icon_32x32@2x.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 72 -resize 128x128 snow.iconset/icon_128x128.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 144 -resize 256x256 snow.iconset/icon_128x128@2x.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 72 -resize 256x256 snow.iconset/icon_256x256.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 144 -resize 512x512 snow.iconset/icon_256x256@2x.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 72 -resize 512x512 snow.iconset/icon_512x512.png
#magick docs/images/snow_icon.png -alpha on -background none -units PixelsPerInch -density 144 -resize 1024x1024 snow.iconset/icon_512x512@2x.png
#iconutil --convert icns snow.iconset

# Set up the .app directory
mkdir -p Snow.app/Contents/MacOS/Libraries
mkdir Snow.app/Contents/Resources

# Copy binary into App
cp $1 Snow.app/Contents/MacOS/Snow
chmod a+x Snow.app/Contents/Macos/Snow

# Copy icons into App
cp docs/images/Snow.icns Snow.app/Contents/Resources/AppIcon.icns

# Fix up Plist stuff
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleDisplayName string Snow"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleIconName string AppIcon"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleIconFile string AppIcon"
PlistBuddy Snow.app/Contents/Info.plist -c "add NSHighResolutionCapable bool true"
PlistBuddy Snow.app/Contents/version.plist -c "add ProjectName string Snow"

PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleExecutable string Snow"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleDevelopmentRegion string en"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleInfoDictionaryVersion string 6.0"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundleName string Snow"
PlistBuddy Snow.app/Contents/Info.plist -c "add CFBundlePackageType string APPL"
PlistBuddy Snow.app/Contents/Info.plist -c "add NSHumanReadableCopyright string Copyright Thomas W. - thomas@thomasw.dev"

PlistBuddy Snow.app/Contents/Info.plist -c "add LSMinimumSystemVersion string 10.15"

# Bundle dylibs
#dylibbundler -od -b -x Snow.app/Contents/MacOS/Snow -d Snow.app/Contents/Frameworks/ -p @rpath -s /Users/runner/work/Panda3DS/Panda3DS/VULKAN_SDK/lib

# relative rpath
install_name_tool -add_rpath @loader_path/../Frameworks Snow.app/Contents/MacOS/Snow
